// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor loop over the control verbs: wait for the worker's turn to
//! end ([`Session::await_turn`]), then either answer an approval box the
//! approval policy answers itself or hand the screen to the manager — once, and
//! exit ([`Session::supervise`]), or as one line per decision while it keeps
//! watching (`Session::watch`). Every request goes through one [`Ctl`] seam
//! (`aterm-ctl` in production, a scripted mock in tests). A request the server
//! did not serve (the hosting instance went away, e.g. a self-update handing
//! sessions to a new instance under the same `@sid`, or the server turned the
//! connection away) does not end the loop: the outage is ridden out
//! ([`Session::ride_out`]) and the loop looks again from a fresh read.
//!
//! A usage limit is a decision point `watch` handles alone: one weekly limit
//! can stop the manager and the worker at once, leaving nobody to act. On
//! `EVENT limited` the watcher never exits on a limit — a budget that would
//! run out before a reset the notice named is stretched past it
//! ([`Session::extend`]) — and the turn-end policy types the continuation
//! 150 s past the reset, a session's share of a minute later (the rules
//! file's text with it), as at any other turn end it acts on (below); a
//! limit the policy does not wait out (`resume_limits = false`)
//! sets the worker's `attention` meta ([`Session::escalate`], once an
//! episode). The episode is
//! [`Episode`]; `supervise` — one look, the manager right there — does none
//! of it ([`Review::unattended`]). An episode ends when the worker
//! works again: a notice that says Claude Code goes on by itself (`⚠ Usage
//! limit reached · continuing automatically at 1:50pm · esc to cancel`,
//! measured 2026-09-17; [`limit::resumes_by_itself`]) is over at the FIRST
//! busy read after it, inside the wait for the turn
//! ([`Session::await_turn_from`]) — the notice row stays on the screen while
//! the worker resumes under it, and the turn it resumes may run for hours
//! (measured: a background shell kept the next point fourteen hours off,
//! the attention badge set all the while) — and one naming a reset when
//! the worker answers on a point that is not the notice
//! ([`Session::look`]: a busy spell after such a notice may be a retry that
//! hits the wall again, and the notice again is the same episode). The
//! same holds after the busy read: the wall again before the worker has
//! answered is the retry that hit it, the SAME episode opened again
//! ([`Retry`], [`Session::open_episode`]), its attention set again — one
//! ask, not one a retry. So does the notice still up after an Autopilot Off
//! park, which took the episode's badge down ([`Session::lower_badges`]).
//!
//! **The approval policy, the wake, the lifecycle.** Every press on an approval
//! box is decided by [`super::policy::approval::decide`] and pressed under the
//! guard it returns: the judged row, anchored, fenced on the judged read's
//! content sequence where the server can (`approval_loop.rs`, `press.rs`);
//! every decision is a row of the approval ledger ([`super::approvals`]). A
//! busy read re-arms `await gone <busy footer>`, so a box that replaces the
//! footer is read the moment it is drawn. `ERR halted` parks the loop until the
//! hold lifts, `ERR busy …`/`ERR rate` back it off; neither ends it. A watch
//! starts by reconciling an attention a previous watcher left and ends by
//! clearing its own (`lifecycle.rs`). A box or question no rule answers is
//! escalated as `<program> <kind>: <subject> (<reason>)` (`escalate.rs`).
//! [`Session::run_hosted`] is the in-GUI host's entry: the same loop under
//! [`SuperviseOpts::hosted`], ended by a stop flag.
//!
//! **The turn-end policy.** Every point where a worker's turn ended (idle, a
//! question, a wall) is decided by the pure
//! [`super::policy::turn_end::decide_turn_end`]. The loop only executes what it
//! returns (`turn_end_loop.rs`): text through the guarded submit (`turn
//! submit=guarded:<the composer row>`), a suggestion through the vendor's
//! accept key and a fenced Enter, a wait as the deadline of the loop's own wait
//! for the screen to move (a change decides again at once). Every act is a
//! ledger row under its rule id.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::approvals::{self, Ledger};
use super::classify::{DEFAULT_PYTHON_ALLOW, Verdict, classify_command_with};
use super::config::Approve;
use super::config::{Autopilot, SupervisorConfig};
use super::fx::Fx;
use super::journal::{CLEARED_MEMORY, CLEARED_TURN_END, Journal};
use super::limit::{self, ResetSpec};
use super::phase::{Phase, busy_signal, last_said_index, last_said_row, status_row};
use super::policy::approval::{Decision, footer_mode};
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

/// One `aterm-ctl` exchange: the exit code, stdout and stderr.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CtlReply {
    /// The client's exit code: 0 ok, 124 a timeout, 1 an error (`ERR …` on
    /// stderr), 75 a request its server went away under
    /// ([`aterm_ctl::EXIT_REPLACED`]).
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CtlReply {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
    pub fn timed_out(&self) -> bool {
        self.code == 124
    }
    /// The server does not know this form of the verb — an older build.
    pub fn usage_error(&self) -> bool {
        self.stderr.contains("ERR usage")
    }
    /// The `ERR …` line, without the client's `aterm-ctl: ` prefix.
    pub fn err_text(&self) -> &str {
        let line = self.stderr.trim();
        line.strip_prefix("aterm-ctl:")
            .map(str::trim)
            .unwrap_or(line)
    }
    /// A bare `ERR` with nothing after it: what a build that predates a verb's
    /// leading option answers when the option is read as a key name.
    pub fn bare_err(&self) -> bool {
        !self.ok() && self.err_text() == "ERR"
    }
    /// The server does not know this form: a usage line or a bare `ERR`.
    pub fn unknown_form(&self) -> bool {
        self.usage_error() || self.bare_err()
    }
    /// An input verb that wrote behind input the worker has not read yet: its `OK` carries
    /// `warn=input-unread input_bytes=<n> input_wait_ms=<ms>` on the reply's first line — stdout
    /// for `key`/`send`/`paste`, the verdict ([`Self::turn_verdict`]) for `turn`, whose stdout
    /// holds the settled rows.
    pub fn warned_input_unread(&self) -> bool {
        let warned = |l: &str| l.split_whitespace().any(|w| w == "warn=input-unread");
        self.ok()
            && match self.turn_verdict() {
                Some(verdict) => warned(verdict),
                None => self.stdout.lines().next().is_some_and(warned),
            }
    }
    /// Whether the reply is `ERR <what>` (`busy sink`, `halted`, …).
    pub fn is_err(&self, what: &str) -> bool {
        !self.ok() && self.err_text().starts_with(&format!("ERR {what}"))
    }
    /// The `seq=<n>` an input verb stamps on its answer (`OK seq=31`,
    /// `OK skipped seq=31`): the content baseline at the write.
    pub fn seq(&self) -> Option<u64> {
        let rest = &self.stdout[self.stdout.find("seq=")? + "seq=".len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }
    /// A guarded write that matched no row: `OK skipped seq=<n>` — nothing
    /// was written. (`OK skipped reason=changed` is a fenced press whose
    /// screen moved: [`Self::skipped_changed`].)
    pub fn skipped(&self) -> bool {
        self.ok() && self.stdout.trim_start().starts_with("OK skipped")
    }
    /// A press fenced on a screen generation (`key if-gen=<epoch>.<seq>`)
    /// that the server skipped because the screen moved past it.
    pub fn skipped_changed(&self) -> bool {
        self.skipped() && self.stdout.contains("reason=changed")
    }
    /// A press fenced on the session's person count (`key if-human=<n>`)
    /// that the server skipped because a person keyed since the read.
    pub fn skipped_person(&self) -> bool {
        self.skipped() && self.stdout.contains("reason=person")
    }
    /// `turn`'s verdict line (`turn submitted=<0|1> status=… seq=…`): the
    /// client prints it on stderr and the settled rows on stdout; a reply
    /// whose first stdout line carries it (a scripted one) is read there.
    pub fn turn_verdict(&self) -> Option<&str> {
        self.stderr
            .lines()
            .chain(self.stdout.lines().take(1))
            .find(|l| l.contains("submitted="))
    }
    /// The turn was submitted: its verdict says `submitted=1`.
    pub fn submitted(&self) -> bool {
        self.turn_verdict()
            .is_some_and(|v| v.split_whitespace().any(|w| w == "submitted=1"))
    }
    /// A guarded turn wrote its Enter and did not see it land: `submitted=0`
    /// with `pressed=<n>`, n ≥ 1 — typed, unverified, and never a turn to
    /// retry blindly (the verb's own words). A screen that never holds still
    /// keeps the echo settle open to the turn's deadline, so the Enter is
    /// written with no time left to see it (Claude Code 2.1.301, measured
    /// 2026-10-16 on the nudges of the since-removed `drive task`: every one
    /// landed so).
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
    /// `turn`'s `seq=<n>`, read from its verdict line (never from the rows).
    #[cfg(all(test, unix))]
    pub(crate) fn turn_seq(&self) -> Option<u64> {
        let v = self.turn_verdict()?;
        v.split_whitespace()
            .find_map(|w| w.strip_prefix("seq="))
            .and_then(|n| n.parse().ok())
    }
    /// The request was not served: the client could not reach the instance hosting the session
    /// or lost it mid-exchange (the connection closed before a reply, a reply cut short, a dead
    /// socket, a reset, a broken pipe, a socket timeout), or the server turned it away before
    /// acting on it ([`TURNED_AWAY`]). Any other `ERR …` line is the server's answer: `ERR no
    /// such session` and `ERR exited` say the session is gone (but see [`Self::unserved`]).
    ///
    /// AND EXIT 75 ([`aterm_ctl::EXIT_REPLACED`], the client's own constant,
    /// never a copy of its number), whatever its note says: `aterm-ctl` exits
    /// so on a request an update's Commit cut and does not ask again —
    /// an `await seq <n>` or `await inbox since=` whose anchor names the old
    /// process, an armed wait, a `turn`, a `post` — or that no successor
    /// answered at all. Read as an answer, a supervising loop (the window
    /// host's) would die at an update instead of riding it out. Nothing else exits 75, and a request that may have
    /// landed (a `turn`, a `post`) is ridden out as one that may have: the
    /// next look reads the screen before it types again, and a lost `post` is
    /// never retried.
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

    /// A request not served ([`Self::lost`]) that the server certainly did nothing with:
    /// turned away unread ([`TURNED_AWAY`]) or never sent ([`UNSENT`]). Any other request not
    /// served was cut mid-exchange (exit 75, [`CUT`]) and may have been carried out: a `post`
    /// landed, a `turn` typed.
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

    /// The client's stderr lines, trimmed and without its `aterm-ctl: `
    /// prefix, the blank ones dropped.
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

    /// The request was not served, as a client riding out an outage reads
    /// it: [`Self::lost`] — or, while `in_outage` (a request was lost and
    /// nothing has been served since), `ERR no such session`. The instance
    /// that adopts the sessions in a handoff may not host this one YET, and a
    /// request it forwards to the instance it replaces falls through to `ERR
    /// no such session` once that one is gone. Outside an outage `no such
    /// session` is the session gone, and final. The one rule for every
    /// client that rides an outage out: the loop (`Session::unserved`).
    pub fn unserved(&self, in_outage: bool) -> bool {
        self.lost() || (in_outage && self.is_err("no such session"))
    }
}

/// What the client prints when a request it sent never got the server's
/// whole answer ([`CtlReply::lost`], and not [`CtlReply::untouched`]: it may
/// have been carried out): `aterm-ctl`'s own words for a connection that
/// closed before the reply or during it, and the OS's for a reset or broken
/// connection and a socket timeout (the Unix and the Windows spellings). A
/// deadline that fires once connected is `aterm-ctl`'s own `no reply from
/// <sock> within <N>s (connected; the server did not answer) — the request
/// timed out`, or `reply from <sock> stopped part-way: … — the request timed
/// out` once its answer had begun, each matched by `timed out`; an older
/// client printed the OS's `Resource temporarily unavailable (os error 35)`
/// for both, which stays here. A reply cut part-way is `server hung up before
/// the complete <line-framed|guarded artifact> response` (an older client:
/// `… complete response`), or `server hung up mid-body` for a byte-framed one.
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
/// What the client prints when a request was never sent ([`CtlReply::lost`] and
/// [`CtlReply::untouched`]): the OS's connect refusal (Unix and Windows spellings), and
/// `aterm-ctl`'s `no answer from <sock> within <N>s: the instance took no request, …` (exit 2).
const UNSENT: &[&str] = &[
    "Connection refused",
    "actively refused",
    "the instance took no request",
];
/// A socket path that is gone, as the client's `connect <path>: …` line says
/// it: the instance that bound it has exited, and the request was never sent.
const LOST_SOCKET: &[&str] = &["No such file or directory", "cannot find the file"];

/// A client line saying its request was never sent ([`UNSENT`], or a
/// [`LOST_SOCKET`] connect).
fn unsent(line: &str) -> bool {
    UNSENT.iter().any(|m| line.contains(m))
        || (line.starts_with("connect ") && LOST_SOCKET.iter().any(|m| line.contains(m)))
}

/// A client line saying its request was cut before the whole answer
/// ([`CUT`]).
fn cut(line: &str) -> bool {
    CUT.iter().any(|m| line.contains(m))
}
/// The server's `ERR` lines that turn a request away before anything is done with it
/// ([`CtlReply::lost`]), each a whole leading phrase: `ERR control server busy; retry` (the
/// admission queue is full), `ERR auth` (the token sent is not this instance's: the `latest`
/// alias moved or the instance restarted; the client re-reads it on its next run), and `ERR
/// main thread stalled <N>s; retry` (a main-thread verb refused before it was posted). The last
/// may also follow a verb's own prefix ([`turned_away`]).
const TURNED_AWAY: &[&str] = &[
    "ERR control server busy",
    "ERR auth",
    "ERR main thread stalled",
];

/// The stalled main thread's refusal as it reads after a verb's own
/// `ERR <what failed>: ` prefix.
const MAIN_THREAD_STALLED: &str = "main thread stalled";

/// Whether an `ERR` line turned its request away unserved: a [`TURNED_AWAY`] phrase leads it,
/// or the stalled main thread's refusal follows a verb's own prefix (`ERR input dispatch
/// failed: main thread stalled 7s; retry`). Either refusal comes before anything is done.
fn turned_away(err: &str) -> bool {
    TURNED_AWAY.iter().any(|&phrase| leads_with(err, phrase))
        || err.strip_prefix("ERR ").is_some_and(|body| {
            body.match_indices(": ")
                .any(|(at, sep)| leads_with(&body[at + sep.len()..], MAIN_THREAD_STALLED))
        })
}

/// Whether `line` starts with the whole `phrase`: the line ends there, or a
/// space or `;` follows it.
fn leads_with(line: &str, phrase: &str) -> bool {
    line.strip_prefix(phrase)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', ';']))
}

/// The transport seam: run one control request against the target session.
pub trait Ctl {
    /// `args` is the request after any `@sid` selector, e.g. `["text", "--json"]`.
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String>;

    /// [`Self::call`] for a request that names no session but asks the
    /// instance hosting `sid` (`who`, read for that session's row). A
    /// transport holding a connection per instance sends it to that session's
    /// host ([`super::RelayCtl`]); every other client sends it as
    /// [`Self::call`] does, to whichever instance that reaches. A wrapper
    /// passes it on when what it wraps can reach other instances.
    fn call_at_host_of(&mut self, sid: &str, args: &[&str]) -> Result<CtlReply, String> {
        let _ = sid;
        self.call(args)
    }

    /// A handle that cuts the request in flight short — and every one after
    /// it — from another thread: what the window's host calls to stop a tab's
    /// loop at once (it hangs the loop's connection up, [`super::RelayCtl`]).
    /// `None` for a client that cannot (`aterm-ctl` per request): the loop
    /// then ends within one step.
    fn interrupter(&self) -> Option<Interrupter> {
        None
    }
}

/// [`Ctl::interrupter`]'s handle.
pub type Interrupter = Box<dyn Fn() + Send + Sync>;

/// One `aterm-ctl` per request: no handle cuts one short ([`Ctl::interrupter`]
/// answers `None`).
impl Ctl for crate::CtlClient {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.run_raw(args)
    }
}

/// THE SESSION LOG'S `act` MARKS a loop owes (session log phase 3B; addendum
/// A17, ruling S28): each one `ledger_row` queued, as the words after
/// `mark -` ([`crate::harness::acts::log_mark`]), sent by
/// [`Session::send_act_marks`]. Best effort, never retried: a server that
/// refuses the verb or a mark's fields latches the producer off for the
/// loop.
#[derive(Debug, Default)]
pub(super) struct ActMarks {
    /// Oldest first, at most [`ACT_MARKS_MAX`]: a loop that cannot send them
    /// drops the oldest.
    pending: Vec<String>,
    /// Latched off ([`act_mark_latches`]).
    off: bool,
}

/// The most `act` marks a loop holds unsent.
pub(super) const ACT_MARKS_MAX: usize = 32;

impl ActMarks {
    /// Queue one mark's words; nothing once latched off.
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

/// Whether a `mark` reply latches the act marks off for the loop (ruling
/// S28): an older server's `ERR unknown verb`, and a refusal of the mark
/// itself (`ERR usage: mark …`, `ERR mark: …`). `ERR busy` and anything
/// else drop the one mark.
pub(super) fn act_mark_latches(reply: &CtlReply) -> bool {
    reply.is_err("unknown verb") || reply.is_err("usage: mark") || reply.is_err("mark:")
}

/// Server features probed once per process (`None` = not yet asked).
#[derive(Debug, Default, Clone, Copy)]
pub struct Caps {
    /// `key if-gen=<epoch>.<seq> …` — the press happens only if the screen
    /// generation is still the judged read's (probed from `help key`, before
    /// the first approval).
    pub gen_fence: Option<bool>,
    /// `key act=approve|deny|answer …` — the press says what it means for
    /// the box it dismisses (session log phase 5, ruling T1), so the box's
    /// close reads `out=approved|declined|answered by=harness`. Read off the
    /// same `help key` reply as [`Caps::gen_fence`]; unknown is not sent.
    pub act_word: Option<bool>,
    /// `turn submit=guarded:<re>` — Enter only while the cursor's row still
    /// matches (learned from the first turn the turn-end policy types).
    pub guarded_submit: Option<bool>,
    /// `send if-gen=<epoch>.<seq> …` — the write happens only if the screen
    /// generation is still the judged read's (probed from `help send`
    /// before the turn-end policy's first fenced write).
    pub send_gen: Option<bool>,
    /// `await agent <word>[,<word>…]` — the server's own agent verdict, pushed as it moves.
    /// Learned from the first screen read at no request of its own: a server whose `text --json`
    /// carries the screen generation (`gen`) publishes the verdict. A refusal of the wait
    /// unlearns it.
    pub await_agent: Option<bool>,
    /// `status input=<word>` measured (not `-`): the host dates the worker's
    /// unread input, so the wait probes it (`stall.rs`; learned from every
    /// `status` the loop reads). Not known yet probes too: only `false`
    /// stops it.
    pub input: Option<bool>,
}

/// A turn's end: the phase it ended in and the screen it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub phase: Phase,
    pub screen: Screen,
    /// The budget ran out with the phase still busy.
    pub timed_out: bool,
}

/// What a host's step at an idle point did ([`IdleHost::at_idle`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostStep {
    /// The step as the journal carries it: `HOST seq=<n> <line>`.
    pub line: String,
    /// The step typed into the agent or ended it: the point is gone (the screen may still show
    /// it; it is not decided again) and the turn it started is the loop's next. `false`: the
    /// point stands, the loop's to decide once the host owns nothing.
    pub moved: bool,
    /// The step typed a turn into the agent (the upgrade's notice, a carry-on)
    /// whose answer is the loop's next point: awaited like the policy's own
    /// acts and counted as no worker work for the continue back-off
    /// ([`super::policy::turn_end::TurnEndState::host_typed`]), and which
    /// turn it was ([`super::policy::turn_end::HostTurn`]: a carry-on's
    /// answer may be a done report, a notice's never is). `None` for a step
    /// that ended the agent and typed nothing.
    pub typed: Option<super::policy::turn_end::HostTurn>,
    /// The step repeats the host's last wait at this point (a person's hold, looked at every
    /// twenty seconds): the journal keeps its first look and the step that ends it, not the
    /// looks between, so they do not push useful rows out of the kept journal.
    pub repeat: bool,
}

/// What the loop knows of the idle point it offers its host
/// ([`IdleHost::at_idle_on`]), read off the turn-end policy as the point was
/// decided.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdlePoint {
    /// THE TASK LANDED UNDER LAND (`docs/DESIGN-harness-autopilot-2026-10-15.md`
    /// §9.4.8; Autopilot Land): the session's word is Land and the policy read
    /// the task landed ([`super::policy::turn_end::TurnEndState::landed`]: a
    /// clean defense at a land act's yield, or a landed record taken on) —
    /// never a done claim alone, which under Land gets the next nudge. The
    /// host types no continuation into it: a relaunched agent's owed carry-on
    /// is settled with nothing typed (`relaunch::resume_as`), and the policy
    /// decides the point as it decides any other. A Land session mid-work
    /// has not landed: its carry-on types nothing of Cruise's either
    /// (`done:landing`, `upgrade_drive::Opts::land`) and leaves the point to
    /// the land policy; every Cruise one gets its carry-on as before.
    pub done_under_land: bool,
}

/// Which of the loop's guards withheld a point from a host that asked for one
/// ([`IdleHost::withheld`]). The first seven are the checks of a break in the
/// agent's own background work ([`Session::host_steps_in_background`]), in
/// order; `NotIdle`, `NotAuthoritative`, `Prompt`, `ActInFlight` and again
/// `Wall` and `LimitEpisode` are those of an idle point
/// ([`Session::host_steps_here`]); `WakeSettle` and `Credits` are both's,
/// asked last. Reporting only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    /// A break is offered only on a busy read.
    NotBusy,
    /// The read is busy with more than the agent's own background work — a
    /// running turn, most often.
    NoBackgroundWait,
    /// A wall is on the screen: a usage limit's notice, a login's, an API
    /// error's.
    Wall,
    /// The break has not stood [`background_settle`] yet.
    Settle,
    /// A limit episode is open ([`IdleHost::limited`]).
    LimitEpisode,
    /// The loop holds for a stall the server published ([`IdleHost::stalled`]).
    Stall,
    /// At a break: an act of the loop's own the agent has not taken yet (no
    /// read since it was typed has seen the agent's live turn,
    /// [`crate::supervise::policy::turn_end::TurnEndState::act_untaken`]).
    ActUntaken,
    /// At an idle point: an act of the loop's own is in flight — typed, and
    /// its point not judged yet.
    ActInFlight,
    /// The point is not idle — a question, a box, a turn.
    NotIdle,
    /// The reader does not vouch for the idle reading.
    NotAuthoritative,
    /// A box is on the screen.
    Prompt,
    /// The Mac woke less than its settle ago ([`Session::settling`]): the network may not be
    /// back, and a turn typed now would meet an API it cannot reach.
    WakeSettle,
    /// USAGE CREDITS CARRY THE SESSION under `[harness] usage_credits =
    /// false` (the credits hold,
    /// [`super::policy::turn_end::TurnEndState::credits_hold_stands`]): a
    /// turn the host types would be billed to them. Offered once the window
    /// they carry past resets, or the screen no longer says so.
    Credits,
}

impl Guard {
    /// Every guard, in the order [`Self::code`] numbers them.
    pub const ALL: [Guard; 13] = [
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
    ];

    /// The word the upgrade's record and `--status` carry (`guard=`).
    #[must_use]
    pub fn word(self) -> &'static str {
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
        }
    }

    /// A small number for an atomic (`1..=13`; `0` is left for none), and
    /// back ([`Self::from_code`]).
    #[must_use]
    pub fn code(self) -> u8 {
        Guard::ALL
            .iter()
            .position(|g| *g == self)
            .and_then(|i| u8::try_from(i + 1).ok())
            .unwrap_or(0)
    }

    /// [`Self::code`] read back; `None` for `0` and any number it never
    /// gives.
    #[must_use]
    pub fn from_code(code: u8) -> Option<Guard> {
        Guard::ALL.get(usize::from(code).checked_sub(1)?).copied()
    }
}

/// A HOLD THE LOOP JOURNALS (`docs/DESIGN-harness-occupancy-2026-10-20.md`
/// §6, the retro's cause 6): where the loop typed nothing though the agent
/// stood still, and why, as its `HOLD seq=<n> …` line says it
/// ([`Session::say_hold`]). Every one was silent before: a point decided
/// NOTHING, and a break in the agent's own background work no host step was
/// offered at — sixteen hours of a journal with no line. Said once per
/// change; the memory is the loop's own, so a successor (a live upgrade's
/// loop at the same screen) says its first again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HoldSaid {
    /// An idle point decided NOTHING ([`TurnEndAction::Nothing`]): the host
    /// withholds the turn ends (`withheld`), nobody runs the loop unattended
    /// (`attended`), or the policy decided it (`policy`).
    Point(&'static str),
    /// A break the agent's own background work holds busy, and no host asks
    /// for a step at it (none, or [`IdleHost::wants`] false).
    NoHostStep,
    /// A break a guard withheld from the host that asked for a step.
    Held(Guard),
}

impl HoldSaid {
    /// The line's words after its `seq=`.
    fn words(self) -> String {
        match self {
            HoldSaid::Point(why) => format!("point nothing why={why}"),
            HoldSaid::NoHostStep => "background no-host-step".to_string(),
            HoldSaid::Held(guard) => format!("background held why={}", guard.word()),
        }
    }
}

/// The loop's wait as told to the host ([`IdleHost::waiting`]): when it ends,
/// as a unix second, the policy's reason (`api-error retry 1`), and its kind
/// ([`TurnEndAction::WaitUntil`]'s `wait`): an API wall's retry wait (the
/// next try) or the short streak's back-off (a pause,
/// `docs/DESIGN-harness-autopilot-2026-10-15.md` §10.5.3). No other wait is
/// told to the host, so `kind` is never [`WaitKind::Other`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitPlan {
    /// Unix seconds at which the loop decides the point again.
    pub at_unix: i64,
    /// The policy's words for the wait.
    pub why: String,
    /// What kind of wait it is: [`WaitKind::Retry`] or
    /// [`WaitKind::BackOff`].
    pub kind: WaitKind,
}

#[cfg(test)]
thread_local! {
    /// A test's NEGATIVE CONTROL of [`Session::tell_wait`]'s "on change
    /// only": set, what was told is forgotten at every step, so every step
    /// tells.
    pub(super) static TELL_EVERY_STEP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A wait as it was last told to the host ([`Session::tell_wait`]): the
/// deadline on the loop's own clock, the unix second it was told as, and
/// its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ToldWait {
    at: Instant,
    unix: i64,
    kind: WaitKind,
}

/// When the turn-end policy decides again at the point still showing, and why
/// ([`Session::turn_end_due`], set by [`Session::set_due`]): the deadline on both clocks, the
/// `Instant` and the unix millisecond it named when set, falling at the first to come
/// ([`Session::due_at`]). A Mac's sleep stops the `Instant` (`CLOCK_UPTIME_RAW`) and never the
/// wall clock, so a wait for a weekly reset ends at the reset however long the Mac slept; a
/// wall clock stepped back delays only the wall time. A due the sleep carried past moves to the
/// wake's settle ([`Session::woke`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Due {
    /// The deadline as the `Instant` it was set as.
    pub(super) at: Instant,
    /// The same deadline on the wall clock, unix milliseconds, as it read
    /// when the deadline was set ([`Session::unix_ms_of`]).
    pub(super) unix_ms: i64,
    /// Why: the policy's words.
    pub(super) why: String,
    /// What kind of wait it is ([`TurnEndAction::WaitUntil`]'s `wait`): a
    /// wall's next try and the short streak's back-off are told to the host
    /// ([`Session::tell_wait`]), no other.
    pub(super) wait: WaitKind,
}

/// The least time between two of the accounts' news that end a latched
/// session's wait ([`Session::account_edge`]): one more ask of a host that
/// refused its relaunch, a minute at the soonest.
const ACCOUNT_NEWS_EVERY: Duration = Duration::from_secs(60);

/// How far the wall second of a wait's deadline ([`Due::unix_ms`]) may move from the second it
/// was told as before it is told again ([`Session::tell_wait`]). Set again awake, the same
/// `Instant` can round to the next second (no news); after a sleep it names a later one (news).
const WAIT_SKEW_S: u64 = 2;

/// What one screen says of the agent's account's usage, by the program's own
/// notices ([`IdleHost::usage_seen`]): usage credits carry the session
/// ([`aterm_phase::credits::in_use`]), and its warning that a window is
/// filling ([`aterm_phase::credits::usage_warning`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageSeen {
    pub credits: Option<aterm_phase::credits::Credits>,
    pub warning: Option<aterm_phase::credits::Warning>,
}

impl UsageSeen {
    /// Whether it says anything.
    #[must_use]
    pub fn says(&self) -> bool {
        self.credits.is_some() || self.warning.is_some()
    }
}

/// WHAT THE LOOP KEEPS OF THE NOTICES THAT SAY USAGE CREDITS CARRY THE
/// SESSION ([`Session::credits_kept`]), from every read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CreditsKept {
    /// The reset the notice they started on named; `None`: none read.
    pub(super) reset: Option<CreditsReset>,
    /// The first read of a notice, on the loop's clock.
    pub(super) since: Instant,
    /// The model a per-model notice named, as a word of its family
    /// ([`super::policy::turn_end::credits_model`]); `None`: the account's.
    pub(super) model: Option<String>,
}

/// The reset the notice usage credits started on named
/// ([`CreditsKept::reset`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CreditsReset {
    /// As printed (`9:43am (America/Los_Angeles)`): the same text is the same
    /// reset, never placed again (a span counts from its print).
    pub(super) text: String,
    /// Placed, in epoch seconds; `None`: it would not place.
    pub(super) unix: Option<i64>,
}

/// Why a claimed `/usage` probe recorded no sample ([`IdleHost::account_probed`]): where the
/// step ended ([`Session::usage_probe`]). Reporting only: the host logs the word and lets its
/// claim go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeMiss {
    /// `/usage` was not submitted: a fence skipped the write (the screen
    /// moved, no row matched), the server turned it away, or the composer
    /// did not show the text as written.
    NotWritten,
    /// It was submitted and no Settings panel came within the bound, or a
    /// turn started instead. No Escape is pressed for a panel nobody saw.
    NotOpened,
    /// The panel never read settled within the bound: a load or a refresh
    /// still in flight, last-known or partial figures, a window missing.
    Unsettled,
    /// The panel's last row is under the grid's fold: what stands right above it cannot be
    /// read, so nothing is said of the visible figures.
    Fold,
    /// A person keyed the session — before the write, or while the panel
    /// was read. Nothing is sampled from a panel a person may be driving.
    Person,
    /// What came up is not the Usage tab under the fullscreen renderer's
    /// rule: another tab, or the inline renderer's panel.
    Renderer,
    /// The panel left the screen by itself before it read settled.
    Closed,
    /// A request's answer never came: what the step did is not known.
    Lost,
}

impl ProbeMiss {
    /// The word the journal's `PROBED … no sample (<word>)` line and a
    /// host's log carry.
    #[must_use]
    pub fn word(self) -> &'static str {
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

/// WHY A HOST DID NOT CLAIM THE ACCOUNT'S `/usage` PROBE
/// ([`IdleHost::account_probe`]), each by its one word ([`Self::word`]) —
/// short, stable, no account's name — which the loop puts on its line about
/// the probe it did not run (`SKIPPED … gate=claim host=<word>`).
///
/// AN OWNED VALUE OF THE LOOP'S CRATE, where the host once answered a
/// `&'static str` of its own: the answer crosses the host's door, so a
/// harness tape holds it and a replay must give the loop back the very
/// refusal — which only a value the replay can make from its word can be
/// ([`Self::of_word`]); a borrowed word of the window's host could only be
/// leaked into being. The reasons are the window's host's (`WorkerIdle`'s
/// claim under its roster's lock), with the trait's default beside them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeNo {
    /// No host keeps accounts here (the trait's default).
    NoAccounts,
    /// `[harness] accounts`, read live, is not `early`: `wall` and `off`
    /// never probe. Nothing was read.
    Mode,
    /// No roster lists an account, the agent is no Claude Code, or nothing
    /// of its process could be read. With no roster nothing was read, made
    /// or locked.
    Unread,
    /// The process is not one the roster's judgments are about (a listed,
    /// stored login in the default home): the figures its panel shows are
    /// no listed account's.
    NotJudged,
    /// Asked afresh under the lock, the roster no longer says a probe is
    /// due: a sample was recorded since, a wall stands on the account, the
    /// owner pinned, the account is the active one still unanswered, or
    /// there is an account to follow.
    NotDue,
    /// A claim stands in the way (`crate::harness::accounts::probe_held`):
    /// this account's within `accounts::PROBE_MIN_S`, or ANY account's
    /// within `accounts::PROBE_APART_S` — the vendor answers a usage read
    /// from the home's one snapshot for a minute, whichever login fetched
    /// it.
    Claimed,
    /// The store's lock stayed held past its bounded wait.
    LockHeld,
    /// The state could not be read or written as this build's.
    State,
}

impl ProbeNo {
    /// Every refusal, in the order [`Self::word`] lists them.
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

    /// The refusal's one word, as the loop's `SKIPPED … host=<word>` line
    /// carries it.
    #[must_use]
    pub fn word(self) -> &'static str {
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

    /// [`Self::word`] read back; `None` for a word it never gives.
    #[must_use]
    pub fn of_word(word: &str) -> Option<ProbeNo> {
        ProbeNo::ALL.into_iter().find(|no| no.word() == word)
    }
}

/// Which rating survey the loop cannot close ([`IdleHost::survey_stuck`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StuckSurvey {
    /// A card the harness will not press: a shape nobody has words for, or a
    /// question row no guard can fence.
    Withheld,
    /// The harness pressed `0` again and again and the card stayed.
    Unclosed,
}

/// What the stall canary's second rung stood on ([`IdleHost::stall_unread`]),
/// and for how many minutes: the witness decides the words a host may say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StallSeen {
    /// A screen no reader names while Claude Code's own title rests on `✳`:
    /// busy on background work alone (90 minutes), or a dialog whose words
    /// have not changed (30). A fault of aterm's own reading.
    Unread { minutes: u64 },
    /// agy's or Muse Code's own background work, which their reader names
    /// (a task, a timer, a subagent, a backgrounded shell, a monitor), under
    /// a turn that ended, run for `minutes` under words that stood still
    /// (90). No title was read and the screen is read: work that has not
    /// ended, no fault of aterm's reading.
    OwnWork { minutes: u64 },
    /// A background call of the agent's own that the host's jobs reading
    /// says has made no progress for `minutes`, the process `pid`
    /// ([`super::background::BackgroundEvidence::NoProgress`]): the screen
    /// is read, and the kernel explains it. Told at once, not at 90 minutes.
    NoProgress { minutes: u64, pid: u32 },
}

/// The loop's knobs ([`Session::run_hosted`]).
///
/// Every policy switch is [`Self::policy`], the `[harness]` table
/// ([`SupervisorConfig`]): the host sets it whole ([`Self::hosted_with`]).
#[derive(Debug, Clone, Default)]
pub struct SuperviseOpts {
    /// The wall-clock budget.
    pub max: Duration,
    /// `python3 <script>` globs that count as reads (empty = the default list).
    pub python_allow: Vec<String>,
    /// The percentage of context left ([`super::phase::context_left`]) at or
    /// below which the loop says `EVENT context seq=<n> <v>% until
    /// auto-compact`, once a descent; afterwards the indicator gone from a
    /// framed screen with no box (or rising by [`COMPACTED_RISE`]) is `EVENT
    /// compacted seq=<n>`. `0` disables both (the default here; the hosted
    /// loop's is [`DEFAULT_CONTEXT_WARN`]). Armed per loop: a compaction
    /// between two loops is not seen.
    pub context_warn: u8,
    /// A file to append one JSON object to for every line the loop prints,
    /// and for the journal-only lines it decides silently
    /// ([`super::journal`]).
    pub journal: Option<PathBuf>,
    /// The approval ledger a hosted loop keeps ([`Session::run_hosted`]),
    /// named by its host (`<state>/drive/<sid>.jsonl`). `None`: no ledger
    /// unless [`Session::set_approval_ledger`] gave one; the loop never falls
    /// back to the process's state directory.
    pub ledger: Option<PathBuf>,
    /// The owner's policy ([`SupervisorConfig`], full power by default). A rating survey is only
    /// ever dismissed (`dismiss_surveys`: a guarded `0`, said as `DISMISSED survey seq=<n>`),
    /// since a rating is a person's to give.
    pub policy: SupervisorConfig,
    /// Behind another holder's supervisor claim, end the loop (`Err` starting
    /// with [`CLAIM_HELD`], naming the holder) instead of watching behind it.
    /// The in-GUI host sets it ([`Self::hosted`]) and restarts a loop when the
    /// claim is released or lapses; off, the loop watches behind it
    /// (`WATCHING …`). The loop's own claim is the only one either way.
    pub yield_when_held: bool,
    /// The host's part at idle points ([`IdleHost`]): takes the live
    /// upgrade's step and a relaunched agent's continuation in place, and says
    /// when the upgrade owns turn ends. `None` for a loop with no host (the
    /// engine's tests).
    pub idle_host: Option<Arc<dyn IdleHost>>,
    /// Where a tasked session's result is snapshotted at the done edge
    /// (`<state>/tasks/`, named by its host; `supervise::task_mark`). `None`: none is
    /// written, and a done task is marked with no result.
    pub tasks: Option<PathBuf>,
}

impl SuperviseOpts {
    /// The in-GUI host's loop: no time budget, the approval policy at full
    /// power, the survey dismissed, every turn end decided by the turn-end
    /// policy, the context indicator watched. Outages are ridden out, and an
    /// unanswerable point is escalated to the worker's attention, the one
    /// hand-over (`await task` wakes on it).
    pub fn hosted() -> Self {
        Self::hosted_with(&SupervisorConfig::default())
    }

    /// [`Self::hosted`] under the owner's `[harness]` policy.
    pub fn hosted_with(cfg: &SupervisorConfig) -> Self {
        Self {
            max: UNBOUNDED,
            context_warn: DEFAULT_CONTEXT_WARN,
            policy: cfg.clone(),
            yield_when_held: true,
            ..Self::default()
        }
    }

    /// The `python3` globs the classifier reads as reads: the configured
    /// ones, else the defaults — what the loop decides every box with, and
    /// what a reader of a tape decides one again with (`tape::explain`).
    pub(crate) fn allow(&self) -> Vec<String> {
        if self.python_allow.is_empty() {
            DEFAULT_PYTHON_ALLOW.iter().map(|s| s.to_string()).collect()
        } else {
            self.python_allow.clone()
        }
    }
}

/// Why a stopped loop's write was refused before the transport
/// ([`Session::call`]). The run ends as a stop, not as this failure
/// ([`Session::run_hosted`]).
const STOPPED_WRITE: &str = "the supervisor was stopped: nothing is written after the stop";
/// Why a stopped loop's wait was refused before the transport
/// ([`Session::call`]): like [`STOPPED_WRITE`], the run ends as a stop.
const STOPPED_WAIT: &str = "the supervisor was stopped: no wait begins after the stop";

/// A budget that never runs out (`--max-s 0`, [`SuperviseOpts::hosted`]).
pub const UNBOUNDED: Duration = Duration::MAX;
/// The hosted loop's `context_warn`.
const DEFAULT_CONTEXT_WARN: u8 = 10;
/// How many rows a screen read asks for when the server can tail — the
/// server's own agent-verdict cut (aterm-gui's `presence::CLASSIFY_ROWS`).
const TAIL_ROWS: usize = 40;

/// A screen as [`Session::limit_words_read_above`] keeps its verdict for: its
/// `seq`, its `first` and its rows.
type ScreenKey = (u64, usize, Vec<String>);

/// Where [`Session::message_cells`] reads the column-0 cell of a row of the
/// rows read: by its grid row, the rows' first at grid row `.0` (`cell <r>
/// 0`), or by its absolute row, the rows' first at absolute row `.0` (`cell
/// abs=<n> 0`, a history row's too).
#[derive(Clone, Copy, Debug)]
enum CellsAt {
    Grid(usize),
    Abs(u64),
}

/// [`Session::read_once`]'s read of the screen ([`Session::read_shaped`]): the
/// screen, the reader a tail that stands chose (`None` for a whole read), and
/// the grid's rows above the screen where the read was whole (`None` for a
/// tail).
struct Shaped {
    screen: Screen,
    read_by: Option<&'static dyn ScreenReader>,
    above: Option<Vec<String>>,
}

/// Whether a `tail=` read cannot stand for the screen and is taken again whole. The grid must be
/// taller than the tail (`first > 0`), and then either:
///
/// * the box `reader` (the session program's) finds has its title row off the
///   rows read ([`aterm_phase::prompt::PromptV2::head_off_screen`]), so a box
///   the pane shows entire is decided by its title;
/// * that box is one whose decision reads every row above it to the screen's
///   top, an `Edit file` box
///   ([`super::policy::box_call::reads_the_screen_above`]): its
///   call's own row can lie above the tail, as on a fresh pane, where the
///   REPL is drawn from the top; or
/// * the tail does not hold the live rows at all: the cursor's row is above
///   it, or every row of it is blank (a dialog drawn wholly above the tail in
///   a tall pane). This test reads the reply alone, no grammar.
///
/// Other reads keep the tail unless [`drawn_above_tail`] or [`gap_above_tail`] says otherwise.
fn tail_misses_the_live_rows(reader: &dyn ScreenReader, screen: &Screen) -> bool {
    screen.first > 0
        && (screen.cursor_row < screen.first
            || screen.rows.iter().all(|r| r.trim().is_empty())
            || reader.prompt(&screen.rows).is_some_and(|p| {
                p.head_off_screen || super::policy::box_call::reads_the_screen_above(&p)
            }))
}

/// Whether a `tail=` read that ends on a blank row cannot stand for the screen: rows above it
/// (`first > 0`), its last row blank (so the live zone, [`aterm_phase::live_zone_start`],
/// begins above the tail), and the reader, handed the tail and the cursor
/// ([`ScreenReader::read_at`]), decides no phase with authority or decides idle. Such a read is
/// taken again whole and cut to the live zone ([`Session::live_zone`]).
///
/// Claude Code's inline renderer draws its REPL at the top of the pane, so on a pane taller than
/// the tail the tail may hold only the box's bottom rule, footer and blank rows; an idle
/// composer's meaning lies in the rows above it. A box the reader decides, or a running turn, is
/// acted on from the tail.
fn drawn_above_tail(reader: &dyn ScreenReader, screen: &Screen) -> bool {
    if screen.first == 0 || screen.rows.last().is_none_or(|r| !r.trim().is_empty()) {
        return false;
    }
    let read = reader.read_at(&screen.rows, screen.cursor_index(), None);
    !read.phase_authoritative || read.phase == Phase::Idle
}

/// Whether a `tail=` read drawn to its last row cannot stand for the screen because the live
/// zone begins above it: rows above the tail (`first > 0`) and a blank gap in it
/// ([`aterm_phase::BLANK_GAP`] rows or more, as [`aterm_phase::live_zone_reaches_above`] walks
/// it, the walk the server's agent verdict takes), as in a fullscreen pane with an API error
/// near the top and the composer at the bottom. Such a read is taken again whole and cut to the
/// live zone ([`Session::live_zone`]). A gap the tail's top row cuts is not seen.
fn gap_above_tail(screen: &Screen) -> bool {
    screen.first > 0 && aterm_phase::live_zone_reaches_above(&screen.rows, TAIL_ROWS)
}

/// The longest single wait, so the budget — and the host's request for an
/// idle point ([`IdleHost::wants`]) — is re-checked between waits.
const WAIT_STEP: Duration = Duration::from_secs(20);
/// How many steps at one idle point that move nothing go back to the host at
/// once for a follow-up ([`Session::host_follows`]) before a step of the
/// wait: the longest run of such steps a lane takes there, each leaving it
/// owning no turn end and wanting the point — a wait a race left wanting
/// one, and the give-up (or a drain's expiry) that comes due behind it, the
/// release that one owes then typed at the next step (it moves the session,
/// and is never counted) — with one more raced wait to spare (F1-R9-L2).
const HOST_FOLLOW_UPS: u8 = 3;
/// The step of the waits a host's request is read between (a turn's, and
/// [`Session::wait_for_next`]'s): [`WAIT_STEP`], never longer than the live upgrade's settle
/// ([`crate::harness::upgrade::quiet_s`]), which a development build may shorten. A shipped
/// binary's settle is no shorter (asserted below).
///
/// Read ONCE, into the session's setup (`SessionSetup::wait_step`), and the
/// loop waits in the setup's step, never this function's: each step is the
/// `timeout` of a recorded `await`, and a replay in a process with another
/// seam must ask the recorded run's.
fn next_wait_step() -> Duration {
    WAIT_STEP.min(Duration::from_secs(crate::harness::upgrade::quiet_s()))
}
// Compared in nanoseconds, not whole seconds: a `WAIT_STEP` of 20.5 s would
// pass `as_secs() <= QUIET_S` and still be cut to 20 s in a shipped binary.
const _: () = assert!(
    WAIT_STEP.as_nanos() <= Duration::from_secs(crate::harness::upgrade::QUIET_S).as_nanos()
);
/// How long the agent's own background work must have been all that ran —
/// its turn over, read so on every look — before the host is offered the
/// break ([`IdleHost::at_background`]): the live upgrade's own settle
/// ([`crate::harness::upgrade::quiet_s`]), so a person who has just read the
/// answer is not typed over.
pub(crate) fn background_settle() -> Duration {
    Duration::from_secs(crate::harness::upgrade::quiet_s())
}
/// How long a box must have shown before the loop presses on it: Claude Code
/// ignores keys within 150 ms of opening a dialog and re-arms that on each
/// refused key. A test against a scripted server sets it to zero
/// ([`Session::set_box_settle`]).
///
/// It MUST STAY ABOVE 250 ms, the limit reset's own refusal (Claude Code
/// 2.1.304, `Lr=250` @234609721 in its bundle, READ): the reset's
/// confirmation drops a choice or an Escape within 250 ms of its mount and of
/// each one it dropped, and the usage-limit dialog re-arms the same window
/// whenever its row list changes — the grant rows arrive after it opens. A
/// changed row list is a changed box key (its options), so this settle
/// starts again from that read; a row reached by arrows is pressed only
/// after `await idle` and on a fresh read fenced by its generation
/// (`press_focused`). That settle answers the vendor's dropped key, not
/// which row to spend: a row drawn while the arrows move is judged again on
/// that fresh read (`press_focused` re-reads the grant row, and a row that
/// is no longer the one the rule takes is `Press::Changed`). Held at compile
/// time below.
const BOX_SETTLE: Duration = Duration::from_millis(300);
const _: () = assert!(
    BOX_SETTLE.as_millis() > 250,
    "the box settle outlasts the limit reset's 250 ms refusal"
);

/// How many spent transcript candidates are kept
/// ([`Session::calls_spent`]).
const SPENT_CALLS: usize = 16;

/// How long Claude Code's autocompact offer must have shown before its first
/// key ([`Session::set_autocompact_settle`]): the dialog refuses every key for
/// 500 ms after it mounts and re-arms a 600 ms refusal on each refused one
/// (2.1.302: `T6e(Ntr=500)`, `Lnt=600`), so [`BOX_SETTLE`] would lose the
/// first key and hold the dialog shut for longer — the vendor's two windows,
/// with margin.
const AUTOCOMPACT_SETTLE: Duration = Duration::from_millis(1200);
/// The server's agent verdict words (`status agent=`: `busy`, `prompt`,
/// `question`, `idle`, `survey`, `wall`, `unknown`, `-`), all of which an
/// `await agent` may name — every one but the point's own
/// ([`Session::agent_wake`]): here an idle point's.
const AGENT_WORDS_BUT_IDLE: &str = "busy,prompt,question,survey,wall,unknown,-";
/// [`AGENT_WORDS_BUT_IDLE`] for a question: every word but `question`.
const AGENT_WORDS_BUT_QUESTION: &str = "busy,prompt,idle,survey,wall,unknown,-";
/// The longest step of an `await agent` at a point ([`Session::agent_wake`])
/// before the content is asked whether it moved
/// ([`Session::moved_under_verdict`]): the server's verdict is a sweep's (at
/// most four a second), and a turn shorter than the sweep — a reply sent at
/// once, a person's prompt answered as fast — never moves it off the point's
/// word, so the wait alone never ends on it (P4 of the forever lab: Gemini
/// CLI's first reply, sent at once, never read in sixteen minutes). The
/// latency of such a turn's point.
const AGENT_RECHECK: Duration = Duration::from_secs(5);

/// How often a question dialog held for the person by the policy (the
/// session's word, or the global switch, `ask`) has the session's `questions`
/// word re-read while it waits ([`Session::wait_for_next`]): the latency of
/// `meta set questions recommended` handing a waiting dialog back.
const HANDBACK_POLL: Duration = Duration::from_secs(2);
/// The longest step of the park while a session's Autopilot is off
/// ([`Session::park_while_off`]): how long a screen that does not move goes
/// before the session's word is read again.
const OFF_STEP: Duration = Duration::from_secs(5);
/// The `<why>` of the `CLEARED seq=<n> attention=<said> <why>` line that takes
/// down a limit's badge adopted from a previous loop once the worker has
/// answered on a point that is not the notice ([`Session::adopted_limit_over`]).
const ANSWERED_PAST_LIMIT: &str = "the worker answered past the adopted limit's notice";
/// The idle window that means "the screen settled".
const IDLE_MS: &str = "2000";
/// The short settle after a press, a review point or a reconnect: the screen
/// holding still this long, or [`SETTLE_CAP`] at most (a busy worker animates
/// and never holds still).
const SETTLE_MS: &str = "500";
const SETTLE_CAP: Duration = Duration::from_millis(1500);
/// How long one read waits on a program's unfinished DEC 2026 frame (`await frame`): past
/// the server's own one-minute hang detector (`aterm_core::terminal::FRAME_HANG`), which
/// answers first, so this is a hang detector for the wait itself, never a latency budget
/// (a live program closes its frame within milliseconds).
const FRAME_WAIT: Duration = Duration::from_secs(61);
/// How many times one read is taken again for a frame still unfinished at the read before
/// it is taken as it stands ([`Session::text_json`]): a program redrawing without pause can
/// open its next frame between the wait and the read, never eight times running when its
/// frames close within milliseconds of opening.
const FRAME_REREADS: u32 = 8;
/// The longest idle wait on a screen without the composer frame before it is
/// read again, so a box that replaces a shell prompt is seen promptly.
const WRITING_CAP: Duration = Duration::from_secs(3);
/// The longest wait on the busy row armed before any screen was read, over
/// every row ([`BusyWait::step`]): as [`WRITING_CAP`], a wait that cannot
/// tell what it waits on is read again within a few seconds.
const UNSCOPED_CAP: Duration = WRITING_CAP;
/// The longest wait on a busy row that STAYS while a box is up
/// ([`ScreenReader::busy_row_outlives_boxes`]: Muse Code's status row over
/// its approval box): the row's leaving cannot say a box was drawn, so the
/// screen is read again this often and a box is answered within it rather
/// than at the end of a whole [`WAIT_STEP`].
const BOXED_BUSY_CAP: Duration = Duration::from_secs(2);
/// How many points higher than its last reading the context indicator must read, after `EVENT
/// context`, for the worker to have compacted: a compaction frees most of the context, so a
/// smaller rise is not taken for one. Unmeasured: how far `/model` to a larger context raises
/// the reading.
const COMPACTED_RISE: u8 = 30;
/// How long a press waits for the worker to take its key (`await seq`)
/// before it reads the screen again.
const STRAY_SETTLE: Duration = Duration::from_millis(2000);
/// `ERR busy sink` is transient (a spill ahead of the frame, another writer):
/// the survey's `0` is retried this many times, this far apart, before it is
/// an error. The approval press is never re-sent on it: the loop backs off,
/// reads and decides again, so a box that changed meanwhile is not answered
/// on the old verdict.
const BUSY_SINK_RETRIES: u32 = 3;
const BUSY_SINK_BACKOFF: Duration = Duration::from_millis(250);
/// Under the safe rules alone (`approve = "safe"`), the same proven box is
/// approved this many times since the last review point, then handed to the
/// manager (a command that keeps failing is not waved through). Full power
/// has no cap (`approval_loop`'s module header).
const MAX_APPROVALS_OF_ONE_COMMAND: usize = 2;
/// How many rows the compact result prints when there is no box.
#[cfg(test)]
const RESULT_ROWS: usize = 28;
/// How many characters of a command, a said row or a notice one loop line
/// carries (a `CHOSE` line's answers too: `crate::harness::stats` reads a
/// line this long as cut).
pub(crate) const LINE_CHARS: usize = 160;
/// How many non-blank rows of the transcript, up to the last thing said,
/// make a review point's words ([`point_key`]) — and, above a box the reader
/// could not read whole (its head off the screen, a question named by its
/// footer or options alone), that box's ([`box_key`]). Above a box read whole
/// only the letters above its echo count ([`ABOVE_LETTERS`]).
const KEY_ROWS: usize = 8;
/// How many letters of the transcript above a box read whole (above the pending call's echo,
/// [`before_the_echo`]) count in its key ([`box_key`]): enough to tell apart output appended
/// before the same box asked again, few enough to fit what a short pane keeps above a box (92
/// above the echo of the measured 80x24 rm box, `cap-taken-box-1`), so a row scrolling in at
/// the top is no change.
const ABOVE_LETTERS: usize = 64;

/// The exit code a loop ([`Session::supervise`], [`Session::await_turn`])
/// ends with on a spent budget.
pub const EXIT_TIMEOUT: u8 = 124;

/// How far past a reset the notice named the budget is stretched
/// ([`Session::extend`]): time for the continuation and the turn it starts.
const RESET_GRACE: Duration = Duration::from_secs(10 * 60);

/// How far the wall clock may run ahead of the loop's
/// monotonic clock between two steps of its wait before the Mac is taken to
/// have slept through it ([`Session::wait_for_next`]).
const SLEPT_THROUGH_S: i64 = 60;

/// Whether the wall clock has run ahead of the monotonic one by more than
/// [`SLEPT_THROUGH_S`] between two marks: the Mac slept in between (`Instant`
/// does not count sleep). A clock stepped back never triggers it.
pub(super) fn slept_through(wall_delta_s: i64, mono_delta_s: i64) -> bool {
    wall_delta_s - mono_delta_s > SLEPT_THROUGH_S
}
/// How far off a limit's reset can be (a weekly limit's, a week): a notice read as further off
/// is misread and stretches no budget ([`Session::extend`]).
const RESET_HORIZON: Duration = Duration::from_secs(8 * 86_400);
/// The turn-end policy's `turn`: as `task`'s nudge, not waited on —
/// `submitted=1` is what counts, what the worker does with it is the next
/// point the loop awaits.
const TURN_IDLE: &str = "idle=600";
const TURN_TIMEOUT: &str = "timeout=2500";
/// The owner key of this loop's attention entry (`meta set attention
/// owner=supervisor …`, a keyed attention entry): a human's badge and this
/// loop's are separate entries, raised and cleared independently.
pub const ATTENTION_OWNER: &str = "supervisor";

/// The owner key of the window's word that an agent CANNOT BE BROUGHT BACK
/// (`aterm-gui`'s relaunch): an entry of its own, so
/// no loop's start ([`Session`]'s reconcile clears this loop's stale
/// entries) and no other agent's relaunch ends it — only its conversation
/// held again does. The upgrade's word has its own key too (`upgrade`).
pub const RELAUNCH_ATTENTION_OWNER: &str = "relaunch";

/// The owner key of THE WORKER'S NEEDS YOU on the person's screen (FOREVER
/// CRUISE / Autopilot Land, `escalate.rs`'s `sync_needs`): `meta set attention
/// owner=needs kind=ask <program> needs you: <first need>[ (+N more)] —
/// carrying on`, raised at the point the worker named them and taken down
/// when they clear, at an Off park and at the loop's end. An entry of its
/// own, so no box's close, busy read or reconcile — which touch
/// [`ATTENTION_OWNER`]'s alone — ends it, and nothing waits on it: the
/// worker carries on. A loop watching behind another supervisor (a proven
/// self-claim's codex-alab raises its own) never writes or unsets it.
pub const NEEDS_ATTENTION_OWNER: &str = "needs";

/// The most one attention entry of this loop's holds: the server's cap for a
/// keyed entry (200 bytes), not the bare field's 256.
const ATTENTION_BYTES: usize = aterm_types::control_verbs::META_ATTENTION_KEYED_MAX;
/// The prefix a limit episode's attention opens with
/// ([`Session::open_episode`]), the one [`Session::close_episode`] unsets. The
/// entry is owned by its key ([`ATTENTION_OWNER`]); a person's entry is theirs.
pub const ATTENTION_PREFIX: &str = "limited:";
/// How much of the box's first line the attention text carries, in terminal cells — a
/// wide character is two ([`approval_line`]).
const APPROVAL_LINE_CELLS: usize = 64;

/// How long a question tab must have been drawn, still, before its first key:
/// Claude Code refuses keys inside a typeahead window after a dialog appears
/// and extends it on each refused key.
const QUESTION_FIRST_KEY_IDLE: &str = "1000";
const QUESTION_FIRST_KEY_CAP: Duration = Duration::from_millis(3000);

/// A fenced press skipped `reason=changed` this many times in a row hands the
/// box to the manager. The row guard alone binds only the judged box's first
/// command row, which a swapped box may also show, so nothing is ever pressed
/// without the generation fence.
const MAX_CHANGED_PRESSES: u32 = 5;
/// The first back-off after `ERR busy …` / `ERR rate`, and the longest.
const REFUSAL_PAUSE: Duration = Duration::from_millis(250);
const REFUSAL_PAUSE_MAX: Duration = Duration::from_secs(8);
/// A refusal not met for this long is news when it comes back ([`Session::refusal_news`]): one
/// that persists is met at every retry, seconds apart; one met again after minutes (a wall's
/// reset, a back-off's rung) is a new appearance.
const REFUSAL_QUIET: Duration = Duration::from_secs(10 * 60);

/// How long an outage is ridden out before it ends the loop, unless
/// `--reconnect-s` says otherwise.
pub const DEFAULT_RECONNECT: Duration = Duration::from_secs(180);
/// The pause before the first try to reach the session again; each pause
/// doubles, up to [`RECONNECT_PAUSE_MAX`], across all of an outage's
/// ride-outs.
pub(super) const RECONNECT_PAUSE: Duration = Duration::from_millis(500);
pub(super) const RECONNECT_PAUSE_MAX: Duration = Duration::from_secs(8);

/// Why a request failed, as the loop needs to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Fail {
    /// The request was not served ([`Session::unserved`]): ridden out
    /// ([`Session::ride_out`]).
    Lost(String),
    /// Anything else — a server answer (`ERR exited`, `ERR no such session`
    /// outside an outage, …), a reply that does not parse, the notes file,
    /// stdout: final.
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

/// The wait before the next read of a turn ([`Session::await_turn_from`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Next {
    /// `await gone <busy row> [rows <a> <b>]` (the program's,
    /// [`ScreenReader::busy_guard`], over the live zone: [`BusyWait`]).
    Gone(BusyWait),
    /// The short settle ([`SETTLE_MS`], at most [`SETTLE_CAP`]).
    Settle,
    /// `await idle 2000`, a step at most.
    Idle,
}

/// The anchored guard of an open box's first row under its header — its
/// command, path or question — for `await gone`: the box leaving the screen,
/// and no other change, ends the wait on it. The box as `reader` reads it.
/// None for a box with no row under its header: a frame half drawn reads
/// as a box of one row (`❯ Continue without using this MCP` alone, the rows
/// under it not yet drawn), whose wait is then on the content moving.
fn box_anchor(reader: &dyn ScreenReader, rows: &[String]) -> Option<String> {
    let (header, esc) = reader.prompt(rows)?.span;
    let row = rows
        .get(header + 1..esc)?
        .iter()
        .find(|r| !r.trim().is_empty())?;
    Some(super::policy::row_guard(row))
}

/// A compiled busy guard's row test ([`Session::busy_up`]): the server's own
/// `aterm_observe::row_matcher`, whose trait this crate cannot name. True
/// while the row is not shown clear of the guard.
type BusyMatch = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// One wait on the program's busy row leaving (`await gone <guard>`): the
/// guard, and the grid rows it is armed over — `rows <a> <b>`, the live zone
/// of the screen it was armed from ([`live_rows`]); `None`, every row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BusyWait {
    guard: String,
    span: Option<(usize, usize)>,
    /// Armed before any screen was read: no live zone was known to scope it
    /// by ([`Self::step`]).
    unscoped: bool,
    /// The row stays while a box is up
    /// ([`ScreenReader::busy_row_outlives_boxes`]): its leaving cannot say a
    /// box was drawn ([`Self::step`]).
    boxed: bool,
}

impl BusyWait {
    /// How long one wait may run: `step`, or [`UNSCOPED_CAP`] at most when
    /// armed before any screen was read, over every row: before a read, a
    /// running turn's busy row and a dead process's footer look alike. The
    /// read the cap brings arms the next wait over the live zone. A row that
    /// stays over a box is waited on [`BOXED_BUSY_CAP`] at most, so the box
    /// is read soon after it is drawn.
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

/// Where the live zone of `screen` starts ([`ScreenReader::busy_zone_start`]), as an index into
/// its rows, and the grid rows a busy-row wait is armed over: from there to the grid's last row.
/// `None` (every row) only when the zone starts at grid row 0 of a whole read; a `tail=` read is
/// always scoped to the rows it returned, since rows above `first` were never seen. Scoping
/// keeps a dead process's busy footer above a new launch card from holding the wait for a whole
/// step. The arming busy row is in the span by construction (`busy_up` over `rows[at..]`).
fn live_rows(reader: &dyn ScreenReader, screen: &Screen) -> (usize, Option<(usize, usize)>) {
    let n = screen.rows.len();
    let at = reader.busy_zone_start(&screen.rows).min(n);
    let span = (at < n && screen.first + at > 0).then(|| (screen.first + at, screen.first + n - 1));
    (at, span)
}

/// Whether a reset on `p`'s wall is placed on the screen's print of it
/// ([`limit::place_reset_with`], a bare clock time re-placed on a new print).
/// Codex's words keep their first placement ([`limit::reset_at_with`]), and so
/// do agy's, Muse Code's and Gemini CLI's, whose walls name no reset that was
/// measured.
pub(crate) fn places_resets_on_print(p: Program) -> bool {
    match p {
        Program::Claude | Program::Generic => true,
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini => false,
    }
}

/// Whether one wait saw the content move past a seq ([`Session::moved_past`]).
#[allow(
    clippy::large_enum_variant,
    reason = "one Past lives on the stack per wait and is matched at once; boxing the \
              screen would put a heap allocation on every wait's read to save nothing"
)]
enum Past {
    Moved,
    /// The step ran out with the content unchanged: the screen the check read
    /// (none once the budget is spent — nothing is read after it).
    Still(Option<Screen>),
}

/// How a ride-out ([`Session::ride_out`]) ended, when it did not end the loop.
enum Rode {
    /// A read of the session answered; `fresh` when it is the outage's first,
    /// `RECONNECTED` said with it.
    Back { fresh: bool },
    /// The budget (`--max-s`, `--timeout`) ran out first: the loop's TIMEOUT.
    Spent,
    /// The hosted loop was told to stop.
    Stopped,
}

/// An outage: from the first request the server did not serve until the loop
/// gets past it ([`Session::call`], [`Session::drive`]). However many
/// ride-outs it takes, it is said once and bounded by one reconnect window.
#[derive(Debug)]
struct Outage {
    /// When that first request came back unserved: the window runs from here.
    since: Instant,
    /// Its kind, the verb and the verb's first word (`await seq`, `key
    /// if=<row>`, `text --json`): a request of the same kind
    /// served again — the probe read aside — ends the outage.
    kind: String,
    /// `RECONNECT` was said.
    told: bool,
    /// `RECONNECTED` was said.
    back: bool,
    /// The pause before the next try to reach the session, doubling across the outage's
    /// ride-outs ([`RECONNECT_PAUSE`]).
    pause: Duration,
}

/// How the shared loop ended. The turn a stop or a timeout carries is the
/// engine tests' `supervise`'s to render ([`Session::supervise`]); the hosted
/// loop says only how it ended ([`watch_end_line`]).
enum End {
    /// The review stopped the loop at this point (a stop-at-review seam).
    Stopped(
        #[cfg_attr(
            not(test),
            expect(dead_code, reason = "rendered by the tests' supervise")
        )]
        Turn,
    ),
    /// The budget ran out; the last screen read.
    Timeout(
        #[cfg_attr(
            not(test),
            expect(dead_code, reason = "rendered by the tests' supervise")
        )]
        Turn,
    ),
    /// The hosted loop's stop flag was set ([`Session::run_hosted`]).
    Quit,
}

/// The loop ended because the session is gone (`ERR exited`, `ERR no such
/// session`): there is no attention left to clear.
fn session_gone(end: &Result<End, String>) -> bool {
    matches!(end, Err(e) if e.contains("ERR exited") || e.contains("no such session"))
}

/// The hosted loop's (and the tests' `watch`'s) last line and exit code for
/// how its loop ended.
fn watch_end_line(end: Result<End, String>) -> (String, u8) {
    match end {
        Ok(End::Timeout(_)) => ("TIMEOUT".to_string(), EXIT_TIMEOUT),
        // `Lines` never stops at a review point.
        Ok(End::Stopped(..)) => ("EXIT stopped at a review point".to_string(), 1),
        Ok(End::Quit) => ("EXIT stopped".to_string(), 0),
        Err(e) => (format!("EXIT {}", exit_reason(&e)), 1),
    }
}

/// THE USAGE ACT A LOOP BEFORE THIS ONE MADE, taken up by a loop that starts
/// on `sid`'s ledger at `path` (a new window, a seamless update, a fault) at
/// `now_ms` (unix ms) — `now` on the policy's clock: the act
/// [`approvals::usage_attempt`] reads back is this loop's own last try at
/// the wall it attaches to, its next the back-off from THAT act
/// ([`TurnEndState::seed_usage_attempt`]), not a second `keep going` the
/// moment it attaches — the continuation's own row or an act typed in its
/// place, at the limit the row names, and never one typed into an account
/// the session has since LEFT; a try a loop read met again since is no try
/// whose work is yet to come ([`TurnEndState::seed_met_attempt`]), its track's
/// appearance where the loop that read it judged it met — the wall's row
/// ([`approvals::UsageAttempt::met_ms`]), or the end of the take window that
/// row opened where it was the try's first read; the last act, a try still
/// inside its take window — the one the first read after it opened, where
/// the ledger shows that read ([`approvals::awaited_try`]) — is awaited, as
/// by the loop that typed it ([`TurnEndState::seed_awaited`]);
/// and it answered every clearing of the owner's made
/// before it ([`TurnEndState::seed_account_resumed`]: `accounts use` on the
/// account the session stood at the limit of — that continuation is typed
/// once a clearing, by whichever loop).
fn seed_usage_act_in(
    state: &mut TurnEndState,
    body: &str,
    sid: Option<&str>,
    older: &OlderReads,
    now_ms: i64,
    now: Instant,
) {
    let ago = |ms: i64| Duration::from_millis(u64::try_from(now_ms - ms).unwrap_or(0));
    // The try itself, still inside its take window: awaited, whatever its
    // limit's track did since ([`approvals::awaited_try`]; an act in its
    // place is the done check's, [`seed_done_check`], or left). A try
    // nothing has read yet is first read by this loop's look, as the loop
    // that typed it would read it then.
    if let Some(t) = approvals::awaited_try_in(body, sid)
        && now_ms >= t.at_ms
    {
        let read = match t.read_ms {
            Some(ms) if !t.ended => FirstRead::Read(ago(ms)),
            _ if t.unread => FirstRead::Unread,
            _ => FirstRead::Unknown,
        };
        let awaited = state.seed_awaited(RULE_LIMIT_RESUME, ago(t.at_ms), read, true, t.met, now);
        // Past its window and TAKEN — its own turn read running, the wall's
        // end its first read or a take after it, no wall read since and no
        // point judged (nor one an older build's journal says it judged,
        // [`OlderReads::judged_ms`]): that turn's end is this loop's first
        // point ([`TurnEndState::seed_try_taken`]).
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
    // The track's appearance: the try, or where the loop that typed it
    // judged it met again — the wall's row, or, where that row was the try's
    // first read, the end of the take window it opened
    // ([`TurnEndState::seed_awaited`]). From the row itself, a successor in
    // that window's last 30 s before the try's next rung took nothing up
    // and waited a fresh 30 minutes (the verifier of 341e1fa81, RS7-7).
    let take_ms = i64::try_from(state.timing.take_within.as_millis()).unwrap_or(0);
    let appearance = match at.met_ms {
        Some(m) if at.read_ms == Some(m) => m.saturating_add(take_ms),
        Some(m) => m,
        None => at.at_ms,
    };
    // A try a loop read met again is taken up however old it is, its rung
    // due in the gap owed ([`TurnEndState::seed_met_attempt`]); so is one
    // nobody read whose answer this loop's first read dates
    // ([`TurnEndState::date_unread_try`]), and where nothing dates it, it is
    // cut at its rung ([`TurnEndState::seed_usage_attempt`]).
    if at.met_ms.is_some() {
        state.seed_met_attempt(ago(appearance), at.limit, now);
    } else {
        state.seed_usage_attempt(ago(appearance), at.limit, now);
    }
    state.seed_limit_conversation(at.conversation, at.program);
    state.seed_account_resumed(at.at_ms.div_euclid(1000));
}

/// [`seed_usage_act_in`] over the ledger at `path`, for a test that seeds a
/// loop from a file it wrote.
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

/// THE DONE CHECK A LOOP BEFORE THIS ONE ASKED, owed its answer
/// ([`approvals::done_check_owed`]), taken up by a loop that starts on
/// `sid`'s ledger at `path` at `now_ms` (unix ms; `now` on the policy's
/// clock): its answer's done report is the check's
/// ([`TurnEndState::seed_done_check`]) — the check itself still inside its
/// take window awaited as by the loop that typed it
/// ([`TurnEndState::seed_awaited`]), its wall row then its window's answer —
/// one typed in a limit's place that nothing has read yet first read by this
/// loop's look, as the limit's continuation is ([`seed_usage_act_in`]).
/// So too an older build's open-items challenge, the check's kin, under its
/// own rule.
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
    // A `/compact` typed at its point: the compaction's continuation types it again.
    if owed.compact {
        state.seed_compact_check(owed.rule);
        return;
    }
    // AN OLDER BUILD'S BOOKS: the points its journal says it judged after the
    // check, no wall cutting it, as [`approvals::done_check_owed_in`] counts
    // the records of a newer one — the first the check's answer (the task
    // ended there, or not), a second a later turn's, after which the check
    // is nobody's to answer; judged and not ended, a done report after it
    // is a first one. Owed still, a successor read a later done report as
    // the check's answer and typed nothing where the loop that asked it
    // asked the check again (follow-5).
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

/// THE LANDING A LOOP BEFORE THIS ONE LANDED, as the files beside the
/// session say it (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.4.5,
/// §9.4.8, §10.2.1; [`TurnEndState::seed_landed`]): the last note of a task's
/// end in `sid`'s journal (`journal`, the one beside its ledger,
/// [`approvals::journal_beside`]) — `DONE …`, or a person's own exchange's
/// `LEFT …` — is `DONE … rule=land@v1`. Under Autopilot Cruise / Land a
/// landing is the one end a loop seeds: an older build's `DONE` of any other
/// rule (the retired token's `finished@v1`, the done check's, the open-items
/// challenge's) reads as nothing ended, and seeds nothing. Nothing its ledger
/// says came after
/// it (`last_ms`, unix ms): no `typed` row (the ledger's last, as
/// [`approvals::typed_at`] reads them) and no later point judged
/// ([`approvals::RULE_POINT`], the end's own written before its note) —
/// `i64::MAX` for a turn read running since ([`approvals::RULE_TURN_RAN`]);
/// and THE END IS STILL THE LAST THING THE JOURNAL SAYS OF THE SCREEN:
/// every point the journal holds after the note ([`said_a_point`]) is the
/// end's own, by the mark of the worker's last words there ([`said_mark`],
/// kept with each point's line) — never by its `seq=`, which a footer tick
/// moves and a seamless update's successor starts over, so the end's point
/// handed over again after an outage (`RECONNECTED`) or a resize is still
/// the end. No box comes after it: a box is drawn only inside a turn.
///
/// A person's or an orchestrator's turn after a landing types nothing the
/// ledger counts as an act, and the policy notes no new end while its task
/// stays done, so such a turn whose reply also said done — the worker's
/// token at the end of THEIR task — was read by the next loop as the
/// landing: `task=landed`, and a `DONE … rule=land@v1` note with no land
/// instruction before that reply (the Together run of 2026-10-16, Claude
/// Code 2.1.302: a person's turn after a landing, then Off, then Cruise with
/// a timer). A turn a loop watched leaves its points and boxes in the
/// journal, and they drop the seed: the new loop decides its first point as
/// any first point is (the token reads done, words alone a first done
/// report, anything else open). A turn NO loop watched is in neither file;
/// the end's mark goes with the seed, and the first point's last words must
/// still be the end's ([`TurnEndState::seed_landed`]).
///
/// `Some` holds the landing's mark — `None` within it where the end's point
/// carries none (a journal a build before the mark wrote): its later points
/// are then the end's own only on the note's `seq=`, and no words are
/// compared at the first point — and the wall time (unix ms) the note was
/// journaled at, which the start face carries to the window
/// ([`TurnEndState::seed_noted_at`]). `journal` is the journal's text, read
/// by the caller through its door; one that holds no such note seeds
/// nothing. Read from the end, and only the lines that name a task's end, a
/// point or a box are parsed.
fn ended_before(
    journal: &str,
    sid: Option<&str>,
    last_ms: Option<i64>,
) -> Option<(Option<u64>, i64, bool)> {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let land = format!("rule={}", super::policy::turn_end::RULE_LAND);
    // The points and boxes said after the last task end, newest first.
    let mut later: Vec<super::journal::JournalRecord> = Vec::new();
    // The landing's note, once read: its `seq=`, when it was noted, and
    // whether it restates the landing after the task's own wake.
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
        // `DONE seq=<n> rule=<id> <why>`, `LEFT seq=<n> rule=<id> <why>`.
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
            // The end's own time: the first note of it, where this one
            // restates it ([`JournalRecord::ended`]), never after this one.
            noted_ms = rec.ended.map_or(rec.t, |first| first.min(rec.t));
            by_wake = rec.line.contains(super::policy::turn_end::WAKE_KEPT);
            note = Some(seq);
            continue;
        };
        // Before the note: the landing's own point, the newest point there
        // (an idle screen's readings between them are none).
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
    // A note with no point before it to read the end's words by.
    let seq = note?;
    later
        .iter()
        .all(|p| !is_box(p) && p.seq.is_some() && p.seq == seq)
        .then_some((None, noted_ms, by_wake))
}

/// THE LANDING A LOOP BEFORE THIS ONE LANDED ([`ended_before`]'s landing):
/// its mark, `None` within where its point carries none.
#[cfg(test)]
fn landed_before(journal: &str, sid: Option<&str>, last_ms: Option<i64>) -> Option<Option<u64>> {
    ended_before(journal, sid, last_ms).map(|(words, _, _)| words)
}

/// A journal line of A BOX ([`said_a_point`]): handed over (`EVENT prompt`)
/// or approved (`APPROVED`).
fn is_box(rec: &super::journal::JournalRecord) -> bool {
    rec.kind == "approved" || rec.phase == "prompt"
}

/// THE LAST POINT A LOOP OF AN OLDER BUILD JOURNALED, where nothing it said
/// of the screen came after it ([`journaled_hold`]): a wait it held there, a
/// task's end, or the point of a continuation it typed with no back-off.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hold {
    /// Why it waited: the words [`decide_turn_end`] gives the wait (empty
    /// where it held none).
    why: String,
    /// The wait's end its line names (unix ms, to the nearest second; the
    /// point's own time where it held none).
    until_ms: i64,
    /// The act the wait ended in followed it, the ledger's last act, and no
    /// point was read since: the act's yield is still to come.
    acted: bool,
    /// The words of the last act journaled before the wait, or of the act
    /// it ended in ([`Self::acted`]): at an API error's wall, the retry
    /// before, whose words tell its cause ([`retry_held`]).
    said: Option<String>,
    /// When its line was written (unix ms): the point it was held at.
    at_ms: i64,
    /// THE SHORT STREAK AT THAT POINT: a short back-off's own (`<k> short
    /// turn(s) in a row`, [`short_held`]), or, at any other wait (a limit's,
    /// a wall's), the streak the last such back-off held, lengthened by the
    /// continuation typed after it ([`is_continue_rule`]), whose yield this
    /// point was: its work the journal does not give, read short — never
    /// a back-off shorter than that loop's (the policy's own
    /// [`TurnEndState::observe`] keeps it through a limit's try, ends it
    /// with a continuation's real work). `None`: no back-off was journaled
    /// before it, or an act typed with none.
    short: Option<u32>,
}

/// WHAT A LOOP OF AN OLDER BUILD HELD AT ITS LAST POINT, read from its
/// journal (`journal`, the one beside `sid`'s ledger,
/// [`approvals::journal_beside`], follow-5). A ledger no
/// point record is in ([`approvals::RULE_POINT`]: every release through
/// 0.106.0 wrote none, and marked no act with the short streak or an API
/// error's rung) says nothing of either, and a loop that followed such a
/// loop counted its back-off from its own attach on the ladder's first rung
/// — up to the ladder's top sooner than the loop before it. That loop
/// journaled each wait it held at a point, in the words every build writes
/// ([`decide_turn_end`], [`wall_action`]'s): `WAITING seq=<n> until=<t>
/// <why>`; each task's end (`DONE …`); and each act, a continuation typed
/// with no wait before it typed at a point no short streak held. The last
/// such point with nothing that loop said of the screen after it — no box
/// handed over or approved ([`is_box`]), no exchange left to a person, no
/// later point that decided nothing the journal says (a point's own line,
/// [`said_a_point`], comes before what it decided there) — is the point it
/// held at, where it came after the ledger's last
/// act (`last_ms`, unix ms); followed by an act and nothing else (its take
/// window's wait is the act's), the act that point ended in, the ledger's
/// last (the act's line written as its row is: [`Hold::acted`]).
fn journaled_hold(journal: &str, sid: Option<&str>, last_ms: Option<i64>) -> Option<Hold> {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    // The wait last journaled (its line's time), and the act it ended in
    // (that act's line's time).
    let mut hold: Option<(i64, Hold)> = None;
    let mut act: Option<i64> = None;
    let mut said: Option<String> = None;
    // The streak at the last point journaled ([`Hold::short`]), and the
    // acts since it: whether one was a continuation, and how many (an act
    // after another with no wait between was typed at a point no back-off
    // held: its streak none — that loop backs off every short streak).
    let mut streak: Option<u32> = None;
    let mut continued = false;
    let mut acts = 0_u32;
    // A POINT READ — its own line journaled (`EVENT idle`, `EVENT limited`,
    // …: [`said_a_point`]; a box ends what was held, below) — whose own
    // wait, act or end has not followed yet: every build journals a point's line BEFORE what it
    // decided there, so what the loop held before it stands until that line
    // says what it held at the point (a wait's streak carried through a
    // limit's try; an act's take window, the act still awaited). A point
    // read after it with nothing of its own between — it decided nothing
    // the journal says — ends what was held; the journal's last, cut before
    // its own line came, leaves it standing. Ended at every point line, a
    // wall's wait lost the short streak across it, and a point read inside
    // an act's take window lost the act the wait still awaited (an API
    // error's retry rung): the follower typed sooner than that loop.
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
            // The act's take window: the act is still the wait's — a point
            // read inside it too.
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
                // A continuation no wait came before was typed at a point no
                // back-off held: its streak none (that loop backs off every
                // short streak), the point unjournaled.
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
            // THE TASK'S END a continuation's done reply made (`DONE …`): a
            // point that lengthens the streak whatever its work, and holds
            // no wait.
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
            // A box, handed over or approved, is drawn only inside a turn:
            // what the loop held at its last point is over.
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
        // The act's line and its row are written in the same step.
        Some(at) => last_ms
            .is_some_and(|a| a.abs_diff(at) <= 5_000)
            .then_some(Hold {
                acted: true,
                ..hold
            }),
        None => last_ms.is_none_or(|a| a < t).then_some(hold),
    }
}

/// A PERSON'S OWN EXCHANGE A LOOP OF AN OLDER BUILD LEFT TO THEM, as it
/// journaled it (`journal`, the one beside `sid`'s ledger,
/// done-5): its last `LEFT …` note with no act after it (`CONTINUED`,
/// `TYPED`: an act closes the exchange, as its row does,
/// [`approvals::open_left_in`]) and no task's end since (`DONE …`), keyed by
/// the note's time ([`approvals::left_before_key`]): the conversation's last
/// turn then is its last turn still while no turn has opened since
/// (`crate::harness::upgrade::LastTurn::names`). A build through 0.105.0
/// notes the exchange left and writes no row naming its turn: unseeded, the
/// first loop after the update read the person's exchange as a turn of the
/// task's and typed the done check or `keep going` into it once.
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

/// THE BOOKS `body` (`sid`'s ledger text) WITH OFF STANDING AT EVERY PARK
/// ITS JOURNAL SAYS (`journal`, the one beside the ledger,
/// done-6): a record of [`approvals::RULE_OFF_STOOD`] at each of the loop's
/// `PARKED seq=<n> autopilot=off` notes ([`lifecycle::parked_off_note`]),
/// before the first row stamped at or after it. A build through 0.105.0
/// parks under Off (`Session::park_while_off`) and notes the park, but
/// writes no record of it: a loop that followed one read the Land episode
/// the park ended as still in hand ([`approvals::land_episode_in`]) and a
/// done check a wall cut before it as still owed its retry
/// ([`approvals::done_check_owed_in`]), where the loop that parked had
/// dropped both ([`TurnEndState::park_off`]). A newer build writes the
/// record beside its note, and the one spliced here says the same again:
/// each reader reads a second record of Off standing as the first. `body`
/// as it is where the journal says no park.
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

/// WHAT A LOOP OF AN OLDER BUILD JOURNALED of the limit's tries and its
/// points, which its ledger does not say (follow-5): read
/// by [`seed_usage_act_in`] where the ledger holds no point record.
#[derive(Debug, Default)]
struct OlderReads {
    /// The acts it saw taken ([`journaled_takes`]).
    takes: Vec<(i64, i64)>,
    /// The points it journaled judging (unix ms), and whether the task ended
    /// there: each wait it held at one (`WAITING …`, no act's take window's)
    /// and each task's end (`DONE …`) — where its rows hold none
    /// ([`approvals::RULE_POINT`], [`approvals::RULE_TASK_DONE`] are newer).
    points: Vec<(i64, bool)>,
    /// The last point it judged after its last act (unix ms): the wait it
    /// held there ([`journaled_hold`]). A try taken before it is no try whose
    /// turn's end is still to come ([`TurnEndState::seed_try_taken`]): that
    /// loop judged it, and its back-off from there is this loop's.
    judged_ms: Option<i64>,
}

/// THE ACTS A LOOP OF AN OLDER BUILD SAW TAKEN, read from its journal
/// (`journal`, the one beside `sid`'s ledger, follow-5):
/// each act whose first wait after it was its take window's (`the worker to
/// take the <rule> act`, [`decide_turn_end`]'s words) — the screen read
/// after it the one it was typed at — with the time of the first other wait
/// after that (unix ms; `i64::MAX`: none), where that loop judged the screen
/// again. An end of the wall between the two was the act's take: its turn
/// began ([`approvals::usage_attempt_taken_in`]).
fn journaled_takes(journal: &str, sid: Option<&str>) -> Vec<(i64, i64)> {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let mut takes = Vec::new();
    // The last act's line, and whether its take window's wait followed it.
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

/// The points a loop of an older build journaled judging ([`OlderReads::points`]).
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

/// The SHORT STREAK a journaled wait held ([`journaled_hold`]): `<k> short
/// turn(s) in a row: <w> s back-off` (or `min`) — `k`, and the back-off `w`
/// (ms).
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

/// The API ERROR'S RUNG a journaled wait held ([`journaled_hold`]), as
/// [`wall_action`] words it — `<the wall> retry <n>` — and the ladder of
/// `timing` it was on: the wall's kind as far as the words say it (the
/// screen's reading replaces it, [`TurnEndState::observe`]), the retries
/// typed before that wait's end (after it, where the act it ended in
/// followed: [`Hold::acted`]), and the appearance the rung counts from
/// (unix ms, half a second late: never sooner than that loop's due). The
/// server's failure and an unreachable API are both `api-error retry <n>`
/// (0.106.0 words them alike, on different ladders): the retry before the
/// wait tells them apart ([`wall_retry_text`]'s `; trying again.` is the
/// network's), and with none to read (the first retry, whose rung both
/// ladders share, or its words lost) the network's shorter ladder is taken
/// — never sooner than that loop. The wait on a measured return (`back but
/// met again`) counts its own acts, which the words do not tell apart from
/// the rest, and is none.
fn retry_held(hold: &Hold, timing: &TurnEndTiming) -> Option<approvals::RetryMark> {
    use aterm_phase::{ApiCause, WallKind};
    let (wall, n) = hold.why.rsplit_once(" retry ")?;
    let n: usize = n.parse().ok().filter(|n| *n > 0)?;
    let api = |cause: ApiCause| WallKind::ApiError {
        code: None,
        retryable: cause != ApiCause::Request,
        cause,
    };
    // The kind, its ladder, and whether the first retry is typed at once
    // (the ladder's rungs then counted from the second).
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

/// A journal line that says the screen reached A POINT OR A BOX of its own
/// ([`ended_before`]): a turn's end handed over (`EVENT idle`, an older
/// build's `--mail` folds `turn` and `idle-no-report`, `question`, `limited`), a box handed
/// over (`EVENT prompt`) or approved under the safe rules (`APPROVED`). A box
/// is drawn only inside a turn, and a point is said again only where the
/// screen's point changed or a turn ran ([`Looking::handed`]). Not the
/// readings and cards an idle screen shows with no turn (`EVENT context`,
/// `EVENT survey`, `DISMISSED`), nor what the loop says of itself.
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

/// `now + max`; a budget no clock can reach ([`UNBOUNDED`]) is a century.
fn deadline_after(now: Instant, max: Duration) -> Instant {
    now.checked_add(max)
        .or_else(|| now.checked_add(Duration::from_secs(100 * 365 * 86_400)))
        .unwrap_or(now)
}

/// Where every line of a loop goes (`watch`'s stdout, `supervise`'s stderr),
/// each recorded in the journal first. One per loop, under a lock its reviews
/// share, so the journal order is the printed order. The lock is held for one
/// line, taken even when poisoned, and nothing else is locked under it.
struct Sink<'w> {
    out: &'w mut (dyn Write + Send + 'w),
    journal: Journal,
    /// Where the journal's one warning goes (the hosted loop: stderr);
    /// `None` sends it to `out` (the tests' `supervise`, whose lines are
    /// stderr already).
    warn: Option<&'w mut (dyn Write + Send + 'w)>,
    /// The loop's clocks ([`Session::fx`]): every journaled line is stamped
    /// by the door's [`Fx::stamp_ms`], so the journal's `t`s are read where
    /// the loop reads its other clocks.
    fx: Fx,
}

impl Sink<'_> {
    /// Record `line` in the journal, its warning to `warn`, else to `out`.
    fn record(&mut self, line: &str) {
        self.record_marked(line, None);
    }

    /// [`Self::record`] of a review point's line, with the mark of the
    /// worker's last words there ([`JournalRecord::mark`](super::journal::JournalRecord::mark)).
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

    /// [`Self::record`] of a task end's note that restates an end a loop
    /// before this one noted, with the time that end was first noted
    /// ([`JournalRecord::ended`](super::journal::JournalRecord::ended)).
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

    /// Record `line`, then print and flush it: a round trip through the door
    /// ([`super::tape::doors::OUT`]), the line out and the write's outcome
    /// back — a line the loop could not print ends it, so the outcome is a
    /// value the loop observes.
    fn line(&mut self, line: &str) -> Result<(), String> {
        self.line_marked(line, None)
    }

    /// [`Self::line`] of a review point's line, its mark journaled with it
    /// ([`Self::record_marked`]); the printed line and its door are the same.
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

/// What the shared loop ([`Session::drive`]) carries from one look to the
/// next.
struct Looking {
    /// The commands approved since the last review point, for the safe
    /// rules' repeat cap ([`MAX_APPROVALS_OF_ONE_COMMAND`]).
    approved: Vec<approval_loop::Approved>,
    /// The first wait of the next turn: the busy footer leaving, except right
    /// after a press, a review point or a reconnect, when it is no signal.
    gone_first: bool,
    /// The review point last handed over, until the worker moves.
    handed: Option<String>,
    /// The screen ([`Screen::seq`]) that point was last read at: a read of
    /// the same point at a moved seq carries the decision's verdicts there
    /// ([`Session::carry_point`]).
    handed_seq: Option<u64>,
    /// The session's submissions and its conversation's mark as that point
    /// was handed over at ([`submit_mark`]): a submission counted since, over
    /// a conversation drawn since, is a turn asked there, whatever its key
    /// says ([`submitted_over`]). `None` from a host that counts none.
    handed_submit: Option<(u64, u64)>,
    /// What the reads since the last turn's end saw of the worker ([`Saw`]).
    /// Kept here, not in one wait's locals, so a busy spell read before a lost
    /// connection still counts after it.
    moved: Saw,
    /// Where the session survey stood at the last look ([`Session::survey`]).
    survey: Survey,
    /// The screen the last look's press read and handed on ([`Step::Decide`]):
    /// the next look's turn, as read.
    read_back: Option<Screen>,
    /// A seeded landing's start face, held back while no bare `meta` was
    /// answered ([`approval_loop::AutopilotWord::answered`]): told by the
    /// first look once its read is answered, under the word it read
    /// ([`Session::look`]).
    face_owed: bool,
}

/// What the reads of a turn saw of the worker ([`Session::await_turn_from`]).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Saw {
    /// A read counted as the worker busy: its busy row, or — the screen with
    /// no composer, no box and no busy row, after a wait that saw no
    /// stillness — output still being written (`writing`).
    busy: bool,
    /// A read showed the worker's own busy row (its live turn), never a screen only `writing`.
    /// A screen only writing is no agent's read: the badge's close at a busy read waits for one,
    /// and a box redrawn through such a read is the box that was up, not the same box asked
    /// again after work ([`Session::look`]).
    live: bool,
}

/// The box escalated to the manager, kept so its badge can be cleared when it leaves
/// ([`Session::close_box`]). One ask per review point ([`Session::escalate_point`]): its key
/// ([`Session::review_key`]) never moves with a footer tick, nor, for a box, with a redraw of
/// it ([`box_key`]).
struct BoxAsk {
    key: String,
    /// The escalation was the memory measure's: the point's own act was
    /// replaced by the person's restart of an agent the machine's trouble was
    /// acted on for ([`Session::mark_memory_badge`]). A measure no longer acted
    /// on hands the point back and takes the badge down ([`Session::memory_edge`]).
    memory: bool,
}

/// What a wait for the turn's end may do about a rating card on its busy reads: the loop's
/// options and the look's survey state ([`Looking::survey`]), lent to the wait so a card that
/// appears during a long busy spell is judged by the same machine. Lent only under
/// `dismiss_surveys`, and never by a loop whose Autopilot is Off.
struct SurveyDuty<'a> {
    opts: &'a SuperviseOpts,
    survey: &'a mut Survey,
}

/// What one step of the survey machine reads besides the turn: the card as the
/// reader found it, and whether the read is a busy one ([`SurveyDuty`]).
struct SurveyCtx<'a> {
    block: Option<&'a aterm_phase::SurveyBlock>,
    busy: bool,
}

/// The session survey, as the loop left it at a look ([`Session::survey`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Survey {
    /// Not open, or not yet said: one open at the next look (with no box up
    /// and no draft typed) has appeared.
    Closed,
    /// Open and said (or handed over): nothing more is said or pressed while
    /// it stays open. A read that shows it gone ends this, however briefly.
    Said,
    /// The survey switch tried its guarded `0`: `pressed` is the server's
    /// stamp on a `0` it wrote, `None` a press it skipped; `tries` the tries
    /// that failed before it. The next look without a box or a draft says
    /// what came of it.
    Tried { pressed: Option<u64>, tries: u32 },
    /// Its `0` did not dismiss it `tries` times: tried again at `at`
    /// ([`SURVEY_RETRY`]), for as long as it stays open.
    Retry { tries: u32, at: Instant },
}

/// The pauses before each retry of the survey's `0` (the last for every retry after), and from
/// which failed try the host is told (no tab is marked) while the tries go on: the turn-end
/// policy acts at no point with a survey open, so a survey handed over for good would silently
/// stop the session being continued.
const SURVEY_RETRY: [Duration; SURVEY_TRIES] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];
const SURVEY_BADGE_AT: u32 = 2;
const SURVEY_TRIES: usize = 4;
/// How long a rating card must have shown before its `0` is pressed: Claude Code drops a digit
/// typed within `wm = 600` ms of the card mounting and leaves it in the composer as text; this
/// is that plus margin, counted from the read that first showed the card
/// ([`Session::survey_young`]).
const SURVEY_SETTLE: Duration = Duration::from_millis(800);
/// How long a `0` the harness wrote may still be waiting on Claude Code: the card takes a digit
/// standing alone in the composer `debounceMs = 400` ms after it appeared, and any change to
/// the composer in that time (a backspace of ours included) cancels the take. The press is not
/// judged, nor its `0` removed, until it has stood this long under a card still open (5x the
/// debounce, so a late timer on a loaded event loop still lands inside it); a `0` still under
/// an open card past it was dropped and is backspaced ([`Session::survey_pending`]).
const SURVEY_LAND: Duration = Duration::from_secs(2);

/// The accounts probe's panel as the loop remembers it
/// (`docs/SPEC-claude-accounts-probe-2026-10-01.md` §2, §3.1; aterm-spec
/// `supervisor_usage_probe_model`'s `ph`): whether a Settings panel the harness opened may be on
/// the screen. Everything but `Idle` is a panel owed its close; while one is owed no second
/// `/usage` is typed and no decision is made on a panel screen ([`Session::usage_panel_look`]).
/// The session ledger carries it too ([`approvals::probe_owed`]), so the next loop takes the
/// panel up; with no such row nothing is ever closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageProbe {
    /// No panel of the harness's is owed.
    Idle,
    /// A panel of the harness's may be open: this loop wrote `/usage` (`own`) and has not seen
    /// the session without it, or the ledger says an earlier loop did. `since` is when this loop
    /// took it up: one taken up from the ledger is closed only once it has stood one person's
    /// grace in this loop's sight (a successor reads no person stamp from before it started).
    Owed { own: bool, since: Instant },
    /// It is open and its Escape did not take `tries` times: again at `at`
    /// ([`SURVEY_RETRY`]'s ladder, the last pause for ever). `told`: the
    /// host was told the panel is stuck ([`PROBE_STUCK_AT`]), once.
    Closing {
        own: bool,
        since: Instant,
        tries: u32,
        at: Instant,
        told: bool,
    },
}

/// What [`Session::usage_probe`] did at a point, for the look that called it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probed {
    /// No probe: a gate was shut, or nothing was written and the screen is
    /// the one judged. The point's own act goes on as decided.
    No,
    /// Nothing more at this look: the write was skipped for a person or a
    /// moved screen, or turned away, and the point is decided again shortly
    /// ([`Session::turn_end_now`]).
    Again,
    /// The probe is over and the panel is not on the screen: the point is
    /// decided again now, through [`Session::turn_end_now`], which asks the
    /// host afresh — so a sample that shows the account low moves the
    /// session at this same point.
    Done,
    /// The panel is still open and owed its Escape: the look returns, and
    /// the next one closes it first ([`Session::usage_panel_look`]).
    Open,
}

/// The bounds of one probe ([`Session::usage_probe`]; a test shortens them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProbeWaits {
    /// How long the panel is waited for after the Enter: half the fenced write's echo wait
    /// ([`turn_end_loop::REACTION_WAIT`]); it is up on the first read after the Enter (55 ms
    /// measured).
    open: Duration,
    /// How long the panel is read for a settled reading: twice the vendor's 5 s fetch timeout.
    settle: Duration,
    /// The longest pause between two reads of the open panel ([`Session::probe_pace`]; the
    /// screen holding still 50 ms ends it sooner). Not the loop's own settle (`await idle
    /// 500`): the whole tab is on an ordinary grid only while its "contributing" block is still
    /// scanning (0.75 s at 60x50 to 2.3 s at 150x50) and under the fold after, so a read that
    /// waits half a second of stillness first can read only the fold. A quarter second puts two
    /// reads inside the narrowest window.
    step: Duration,
    /// How long the panel is given to leave after its Escape (`await gone` on its header row):
    /// [`SURVEY_LAND`]; measured gone within 35 ms.
    close: Duration,
}

const PROBE_WAITS: ProbeWaits = ProbeWaits {
    open: Duration::from_secs(5),
    settle: Duration::from_secs(10),
    step: Duration::from_millis(250),
    close: Duration::from_secs(2),
};

/// From which failed Escape on the host is told the panel is stuck
/// ([`IdleHost::inform`], once) while the tries go on: as
/// [`SURVEY_BADGE_AT`].
const PROBE_STUCK_AT: u32 = 2;

/// A limit reset's step as the loop remembers it
/// (`docs/SPEC-claude-accounts-resets-2026-10-02.md` §2, §3.1; aterm-spec
/// `supervisor_limit_reset_model`'s `ph` and `own`): whether the harness took the usage-limit
/// dialog's grant row and has not yet told its host how that ended. While one is owed it is
/// dealt with first at every look ([`Session::reset_look`]), no second row is taken, and no
/// approval rule sees the pending line or the confirmation. The session ledger carries it too
/// ([`approvals::reset_owed`]), so the next loop takes the step up; with no such row a
/// confirmation on the screen is a person's, and is not touched.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResetStep {
    /// No step of the harness's is owed.
    Idle,
    /// The row was taken and its end has not been told.
    Owed(Box<ResetOwed>),
}

/// What the loop holds of a limit reset's step that is owed
/// ([`ResetStep::Owed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResetOwed {
    /// The host's go for the row this loop took and saw taken: what the
    /// confirmation is held to. `None`: the ledger says an earlier loop took
    /// the row, or this loop's own press got no answer — a step that is
    /// closed, never entered.
    go: Option<ResetGo>,
    /// When the host was asked for the claim this step stands on (from just
    /// before its first `Take`); for a step taken up, when this loop took it
    /// up.
    since: Instant,
    /// When the step's last key was written — the row's Enter, then the
    /// confirmation's Enter or an Escape: no bound on the vendor's next
    /// screen is counted from earlier than this ([`Self::shown`]).
    keyed: Instant,
    /// The confirmation's Enter was written (or its answer never came, or a
    /// read showed the claim under way).
    entered: bool,
    /// How many Enters were written on the confirmation
    /// ([`reset_loop::RESET_ENTERS`] at most).
    enters: u32,
    /// Why the loop kept the reset, once it did: from then on nothing but an
    /// Escape goes.
    kept: Option<KeepWhy>,
    /// How many Escapes did not close the dialog, and when the next may go
    /// ([`SURVEY_RETRY`]'s ladder, the last pause for ever).
    tries: u32,
    at: Instant,
    /// The host was told the confirmation will not close
    /// ([`reset_loop::RESET_STUCK_AT`]), once.
    told: bool,
    /// When this loop first read the confirmation: nothing is keyed on it
    /// before it has stood the setup's box settle from there (the vendor
    /// refuses a key sooner, and re-arms on it), and a step taken up waits
    /// one person's grace from there.
    seen: Option<Instant>,
    /// What a confirmation that read exactly offered: the offer the host
    /// records.
    offer: Option<ResetOffer>,
    /// Until when the step's key is left to a person's hand: the `HELD` line
    /// is said once a hold.
    hold: Option<Instant>,
    /// What the step's last read showed, and since when its reads have
    /// shown that without a break: what each bound on a screen that does
    /// not move is counted from (the later of this and [`Self::keyed`]) — a
    /// pending line first seen long after the step's last key (a person's
    /// own Enter, an outage) has its whole bound from there.
    shown: Option<(ResetShown, Instant)>,
    /// The confirmation's yes is in the session ledger and the journal: once
    /// a step, with the first Enter that was written or may have been.
    yes_said: bool,
    /// How many Enters in a row the server skipped for a screen that had
    /// moved under the read ([`reset_loop::RESET_SKIPS`] at most).
    skips: u32,
    /// How many times in a row the host could not say, asked again before
    /// the Enter ([`reset_loop::RESET_AGAIN_MAX`] at most).
    agains: u8,
    /// The open row this step was taken up from was older than a step can
    /// be ([`reset_loop::RESET_STALE`]): its end is told to nobody.
    stale: bool,
}

/// What a read of a limit reset's owed step showed, as its bounds tell one
/// screen from the next ([`ResetOwed::shown`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResetShown {
    /// `Checking for a reset…`.
    Checking,
    /// `Resetting your limits…`.
    Resetting,
    /// The usage-limit dialog, still.
    Wall,
    /// The confirmation, or a box the step takes for it.
    Confirm,
    /// Another box.
    Other,
    /// Nothing the readers read.
    Unread,
}

/// Where the loop's watch on the context indicator
/// ([`SuperviseOpts::context_warn`]) stands
/// ([`Session::watch_context`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    /// Not warned: the first reading at or below the threshold is said.
    Armed,
    /// Said (`EVENT context …`), `last` the latest reading since. The
    /// indicator gone from a framed screen with no box up, or reading
    /// [`COMPACTED_RISE`] points or more over `last`, is the compaction: said
    /// once, and the watch is armed again.
    Warned { last: u8 },
}

/// A limit episode: from the `EVENT limited` that opened it until the worker
/// works again ([`Session::limit_after`], [`Session::limit_on_busy`]). When
/// to continue is the turn-end policy's ([`Session::turn_end_due`]); the
/// episode is what the person is told.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Episode {
    /// The read the `EVENT limited` that opened the episode was of — the
    /// escalation's seq; an episode opened again after a busy read
    /// ([`Retry`]) keeps the first one's.
    seq: u64,
    /// The notice says Claude Code goes on by itself
    /// ([`limit::resumes_by_itself`]): the first busy read after it is the
    /// continuation, and closes the episode ([`Session::limit_on_busy`]).
    resumes_itself: bool,
    /// The reset the notice named, Unix seconds (`None`: it named none the
    /// watcher could read).
    reset_unix: Option<i64>,
    /// The notice's `reset=` text as read (`-` for none): the `EXTEND`
    /// line's, and the reset's identity — the same text read again is the
    /// same reset ([`Session::refresh_reset`]).
    reset_text: String,
    /// `EXTEND` was said for this reset.
    extended: bool,
    /// The episode was escalated: opened on a wall the turn-end policy did
    /// not act on at once, or found waiting on one since.
    raised: bool,
}

/// An episode closed at a busy read ([`Session::limit_on_busy`]) or by an Autopilot Off park
/// ([`Session::lower_badges`]), kept until the worker answers on a point that is not the notice
/// ([`Session::look`], or a read of the park, [`Session::park_while_off`]). The wall again
/// before that (the retry hitting it, or the notice the park left up) reopens the same episode
/// ([`Session::open_episode`]): attention set again, no second mail.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Retry {
    /// [`Episode::seq`] of the episode that closed.
    seq: u64,
}

/// What [`Session::auto_read`] made of a turn.
enum Step {
    /// A box was answered (approved, or declined with a reason), or had left: look again.
    /// `settle` makes the first wait the settle wait, not `await gone` (the busy footer is not
    /// up yet right after a press).
    Again { settle: bool },
    /// A review point: the manager's.
    Review(Turn),
    /// The next look's turn is this screen, already read: the policy text read
    /// back exactly after the screen held still
    /// ([`Session::press_question_text`]). The typed text mounts no new box,
    /// and the Enter decided on it is fenced on its generation, so a screen
    /// that moved since skips it (`reason=changed`) and is decided again.
    Decide(Screen),
}

/// The half of the loop that differs between the hosted loop (and the
/// engine tests' `watch`, [`Lines`]) and the engine tests' stop-at-review
/// seams.
trait Review {
    /// A read-only prompt was approved; the press landed at `seq`.
    fn approved(&mut self, seq: u64, command: &str) -> Result<(), String>;
    /// A new review point. `false` stops the loop with it (the tests'
    /// `supervise`); `true` means it was reported and the loop keeps watching.
    fn review(&mut self, turn: &Turn) -> Result<bool, String>;
    /// [`Self::review`] of a point whose worker's last words mark `mark`
    /// ([`said_mark`]): a half that journals its points keeps the mark with
    /// the point's line, which the next loop compares a landing with
    /// ([`ended_before`]). One that journals none reviews it as any point.
    fn review_marked(&mut self, turn: &Turn, mark: Option<u64>) -> Result<bool, String> {
        let _ = mark;
        self.review(turn)
    }
    /// An informational line (`RECONNECT …`, `RECONNECTED …`), said as it
    /// happens.
    fn say(&mut self, line: &str) -> Result<(), String>;
    /// A line for the journal alone (`ESCALATED …`, `CLEARED …`, `WAITING …`):
    /// recorded, never printed, so the lines a harness reads are the same
    /// with and without a limit.
    fn note(&mut self, line: &str);
    /// [`Self::note`] of a task end's note (`DONE …`), with the time the end
    /// it restates was first noted where it restates one a loop before this
    /// one noted ([`TurnEndState::restated_noted_ms`]): a half that journals
    /// its notes keeps it with the note's line, which the next loop restates
    /// the end with ([`ended_before`]). One that journals none notes it as
    /// any line.
    fn note_ended(&mut self, line: &str, ended: Option<i64>) {
        let _ = ended;
        self.note(line);
    }
    /// Whether this half runs unattended (the hosted loop): a limit is then
    /// the loop's to handle ([`Session::turn_end_at_point`]). A stop-at-review
    /// seam stops at the limited point and does none of it.
    fn unattended(&self) -> bool;
}

/// The engine tests' bare wait ([`Session::await_turn`]): no loop around it —
/// nothing is said, noted or handled; a limit is the caller's.
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

/// The engine tests' `supervise` ([`Session::supervise`]): the first review
/// point ends the loop, and is its result; an informational line goes to the
/// sink (stderr), never into the result. The journal gets every line said,
/// and the line [`Lines`] would print for an approval and for the review
/// point.
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

/// The hosted loop's (and the engine tests' `watch`): one flushed line per
/// approval and per review point, each journaled before it is printed.
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

/// The line the loop prints for an approval: `APPROVED seq=<n> <command>`.
fn approved_line(seq: u64, command: &str) -> String {
    format!("APPROVED seq={seq} {}", clip(command))
}

/// What one guarded press did. `seq` is the content baseline the server
/// stamped on its answer (the screen read before it, when it stamped none):
/// the caller waits for the screen to move past it before looking again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Press {
    /// The answering key was written while the box was up: the `1`, a
    /// dialog's Enter, or a decline's Enter on its text.
    Pressed { seq: u64 },
    /// Nothing was written: the box was no longer on the screen at the check.
    Skipped { seq: u64 },
    /// Nothing was written: the press was fenced on the judged read's
    /// content sequence and the screen had moved past it
    /// (`OK skipped reason=changed`).
    Changed { seq: u64 },
    /// A question's focus move was written and no read has shown it land
    /// yet ([`Session::press_question`]): nothing more is sent until one does
    /// (the loop's progression rule, `approval_loop.rs`).
    Unseen { seq: u64 },
    /// A person gave the session input while a question's answer was keyed
    /// ([`Session::press_question`]): nothing more was sent, and the loop waits
    /// for their quiet and decides again.
    Yielded { seq: u64 },
    /// A question's policy text was written into its free-text row and read
    /// back exactly as written ([`Session::press_question_text`]): no Enter
    /// was pressed — that is a later decision, on a read that shows the field
    /// holding it (`approval_loop.rs`). `seq` is the read that showed it.
    Typed { seq: u64 },
}

/// A guarded press, and whether the connection held until it was seen
/// through.
#[derive(Debug)]
enum Pressing {
    /// The press was decided and seen through.
    Done(Press),
    /// The connection was lost first: the press got no answer, so its key
    /// may or may not have been written; `why` is the failure.
    Lost { why: String },
    /// Nothing was written: the session is held (`ERR halted …`).
    Halted { why: String },
    /// Nothing was written: the server turned the press away for now (`ERR
    /// busy …` — another writer, a lease, a sink that cannot take the frame
    /// — or `ERR rate`).
    Refused { why: String },
    /// The choice could not be made as decided (a focus move with no
    /// generation fence, a focus that did not land, a decline whose next state
    /// did not show), with `why`. `retry` when a later try may land it (full
    /// power tries again); otherwise the box is handed over. Nothing written
    /// chose an option: an arrow moves focus, and a decline's Tab and text
    /// only fill the refusal input, which only its Enter submits.
    Unconfirmed { why: String, retry: bool },
}

/// What a key under a row guard did ([`Session::guarded_key`]): the survey
/// switch's `0` ([`Session::dismiss_survey`], whose words these are) and the
/// accounts probe's `esc` under the Settings panel's header row.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dismiss {
    /// The `0` was written while the survey was up; `seq` is the server's
    /// stamp (the screen read before it, when it stamped none).
    Pressed { seq: u64 },
    /// Nothing was written: no row matched at the check — the survey had
    /// left the screen first, or its row did not match (the next look finds
    /// it still open and hands it over).
    Skipped,
    /// Nothing was written: a person gave the session input since the read (`OK skipped
    /// reason=person`, the `if-human=` fence). No try was spent; the next look reads again, and
    /// their grace holds the card.
    Yielded,
    /// Nothing was written: the session is held (`halted`) or the server
    /// turned the press away for now; the survey is tried again after.
    Refused { halted: bool, why: String },
}

/// What a backspace for a digit the loop left in the composer did
/// ([`Session::backspace`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Backspaced {
    /// The server wrote it: the digit is gone.
    Done,
    /// Nothing was written: the session is held (`halted`) or the server turned the key
    /// away for now (`busy`, `rate`). The digit still stands and its removal is owed.
    Refused { halted: bool, why: String },
}

/// One driven session: the transport, what it was given before its loop
/// runs ([`SessionSetup`]: the target selector, the holder, how long an
/// outage is ridden out, …), the probed caps, and what the loop carries
/// across an outage and from one look to the next.
pub struct Session<'a, C: Ctl> {
    ctl: &'a mut C,
    /// Everything the loop was given before it ran, set through the setters
    /// below and read back whole by a tape's header ([`SessionSetup`]); the
    /// rest of this struct is what the loop learns as it runs.
    setup: SessionSetup,
    caps: Caps,
    /// The outage in progress: from a request the server did not serve until
    /// the loop gets past it.
    outage: Option<Outage>,
    /// A ride-out's probe read is in flight: its answer does not end the
    /// outage (it says the session answers a read, not that the request that
    /// failed would now be served).
    probing: bool,
    /// The last screen a read returned: the TIMEOUT's when the budget runs
    /// out while an outage is ridden out.
    last: Option<Screen>,
    /// The busy guard last compiled ([`Self::busy_up`]) and its matcher:
    /// one compile per guard, not one per busy read.
    busy_match: Option<(String, BusyMatch)>,
    /// A screen read since the loop last looked at the session survey showed
    /// none open ([`Self::screen`], [`Self::survey`]) — a busy read on the
    /// way to a turn included, so a survey that leaves and comes back while
    /// the worker is busy is a new appearance.
    survey_gone: bool,
    /// The context indicator's threshold for the loop in progress
    /// ([`Self::watch_context`]), set by [`Self::drive`] from its options:
    /// `0` — off — until then, so a bare wait says nothing about it.
    context_warn: u8,
    /// Where the watch on the context indicator stands.
    context: Context,
    /// The limit episode in progress, if any.
    limit: Option<Episode>,
    /// The episode a busy read or an Off park closed, until the worker answers ([`Retry`]): the
    /// notice again before that opens it again.
    retry: Option<Retry>,
    /// The box escalated to the manager, until it leaves the screen
    /// ([`Self::escalate_point`]).
    box_ask: Option<BoxAsk>,
    /// The loop's door to its clocks: every monotonic and wall read and every
    /// sleep of the loop goes through it ([`super::fx`]; the `supervise-fx`
    /// source guard keeps a direct one out). The system's in the shipped loop;
    /// a test's fixed clock ([`Self::set_clock`]) or virtual one
    /// ([`Self::set_fx`]) in tests.
    fx: Fx,
    /// The local zone's offset, read once from `date` when first needed.
    local_offset: Option<i64>,
    /// A test's fixed local zone offset ([`Self::set_clock`]): the ANSWER
    /// the `probe.tz_offset` door gives in place of `date`, never a way
    /// around the door — a replay, which has no test's stand-ins, must cross
    /// the same door at the same read the recorded run did.
    local_offset_fixed: Option<i64>,
    /// A zone's offset by name at an instant ([`limit::zone_offset_at`]; a
    /// table in tests), read only through [`Self::zone_lookup`] — the loop's
    /// door, `probe.zone_offset` on a harness tape.
    zone_offset: fn(&str, i64) -> Option<i64>,
    /// What the turn-end policy remembers of this session
    /// ([`super::policy::turn_end::TurnEndState`]).
    turn_end: TurnEndState,
    /// When the turn-end policy decides again at the point still showing
    /// (its `WaitUntil`), and why — the deadline of the loop's wait for the
    /// screen to move ([`Self::wait_for_next`]), on both clocks ([`Due`]).
    turn_end_due: Option<Due>,
    /// The two clocks as the loop last read them (monotonic, and wall in unix ms): the marks of
    /// its sleep watch ([`Self::watch_sleep`]), read after every request and at each step of a
    /// wait, whatever point shows.
    clock_marks: Option<(Instant, i64)>,
    /// THE SLEEPS OF THE MACHINE THIS LOOP KNOWS OF, on the wall clock
    /// ([`SleepBook`]): every one its watch measured ([`Self::watch_sleep`]),
    /// and what the system's record and the ledger said at its first look of
    /// the sleep before it ([`Self::sleep_count`]). Any turn's done row is
    /// read less the sleep inside the turn's span ([`Self::turn_asleep`]).
    sleeps: SleepBook,
    /// The machine's count of its sleep as this loop last read it
    /// ([`super::fx::Fx::asleep`]): at the loop's first look, and again
    /// where the watch suspects a sleep ([`Self::recount_sleep`]). `None` on
    /// a machine that keeps no count, whose sleep the watch reads by its
    /// clocks alone, and before the first look.
    sleep_count: Option<SleepStamp>,
    /// THE LOOP'S FIRST LOOK AT THE MACHINE'S SLEEP ([`Self::look_at_sleep`]):
    /// when (unix ms), the count then, and the system's record of its last
    /// sleep — what the seed sets the ledger's last act against
    /// ([`Self::seed_sleep`]). `None` before it.
    sleep_look: Option<FirstLook>,
    /// WHY THE BOOKS COULD NOT SAY how long the machine slept since their
    /// last act ([`Self::seed_sleep`], `SinceAct::unknown`): kept until a
    /// turn's done row reaching back past this loop's first look is read
    /// ([`Self::turn_asleep`]), which reads it as before the count, whole of
    /// that sleep but for the system's record of its last; then owed to the
    /// journal, once ([`Self::sleep_unsaid`]).
    sleep_unknown: Option<&'static str>,
    /// ...owed, and said with the point's execution ([`Self::say_sleep_unknown`]).
    sleep_unsaid: Option<&'static str>,
    /// A sleep the watch measured whose wake [`Self::woke`] has not yet
    /// looked at for a due the sleep carried past ([`Self::check_slept`]): the
    /// wall clock when it was measured, unix seconds.
    slept_at: Option<i64>,
    /// The wake's settle, placed where the watch measured the sleep ([`Self::settle_wake`]):
    /// until it falls no turn is typed into the agent (no policy act,
    /// [`Self::turn_end_execute`]; no host step, [`Guard::WakeSettle`]; no accounts probe,
    /// [`ProbeGate::WakeSettle`]) and a point the policy acted on is decided again then; and
    /// whether its `WAITING … the Mac woke` has been said.
    wake_settle: Option<(Due, bool)>,
    /// A restart the host could not make yet, in a row
    /// ([`Self::restart_agent`]): each next try waits longer.
    restart_waits: u32,
    /// THE RUN OF RESTART TRIES standing: its kind ([`Restart::word`]) and
    /// when it began, its first ask, unix seconds ([`Self::restart_agent`]).
    /// A relaunch any ask of the run made had its line typed no sooner
    /// ([`TurnEndState::fallback_made_at`]). A restart of another kind is a
    /// run of its own: kept across kinds by the count of waits alone, a
    /// memory restart answered `wait:held` left its first ask on the
    /// fallback's relaunch asked fifteen minutes later, the bucket's notice
    /// that answered a person's turn between the two read as written after
    /// the relaunch, and its redraw as the wall: the bucket's reset waited
    /// out, two days, where the session ran on the fallback (the verifier of
    /// 5b72b1613, R5-3). `None`: no run stands.
    restart_run: Option<(&'static str, i64)>,
    /// When the loop looks again at the point still showing, whatever the
    /// turn-end policy says: a try of the survey's `0` that is owed
    /// ([`Survey::Retry`]). Ends the loop's wait ([`Self::wait_for_next`]).
    look_at: Option<Instant>,
    /// When a read FIRST showed the rating card now open (`None`: none is, or none
    /// has been seen since the last read that showed it gone); how long it must
    /// have shown before its `0` goes is the setup's ([`SURVEY_SETTLE`]).
    survey_first_seen: Option<Instant>,
    /// When the harness's `0` for a rating card went in, until it is accounted for — the
    /// card took it, a backspace removed it, or a read past [`SURVEY_LAND`] shows no
    /// lone `0` in the composer; how long the card is given to take it is the setup's
    /// ([`SURVEY_LAND`]). The press is judged, and a lone `0` removed, by this and not by
    /// the survey machine's state, so a `0` is never forgotten in one of its states.
    survey_zero: Option<Instant>,
    /// The survey the host was told it cannot close is up ([`Session::survey`]): it
    /// goes with the card, not with the point (an idle point is the same point with or
    /// without a card over it, so the point's own clearing would leave it standing).
    survey_told: Option<StuckSurvey>,
    /// The host was told, once, that this session sees no survey
    /// ([`Self::survey_absent`]): a successor loop that never saw the card its
    /// predecessor told (a body restart, an update) still ends that fact.
    survey_absent_told: bool,
    /// Until when a busy read leaves the survey to a person's hand
    /// ([`Session::survey_held_busy`]); the idle look uses [`Self::held_for_person`].
    survey_hold: Option<Instant>,
    /// The kind of the card the last `0` was pressed on, for the `DISMISSED` line.
    survey_kind: Option<aterm_phase::SurveyKind>,
    /// The turn running now as the loop's reads keep it: its span (the
    /// first busy read since the last review point — the turn's work, as the
    /// loop saw it, [`TurnEndReading::worked`]).
    running: super::policy::turn_end::RunningTurn,
    /// A wall's reset as read, by its notice's text: the same text is the
    /// same reset (a span counts from the print, not from every read).
    wall_reset: Option<(String, Option<i64>)>,
    /// The stuck-busy canary ([`wall_memory`]): when it last asked the server
    /// what the agent's own title says (how long a screen must read busy on
    /// background work alone before it does is the setup's).
    stuck_asked: Option<Instant>,
    /// How far up the canary's ladder this stall has climbed
    /// ([`wall_memory::StuckRung`]), and since when its third rung stood, of
    /// which kind (the end's `CLEARED` line names it).
    stuck_rung: wall_memory::StuckRung,
    stuck_raised: Option<(Instant, super::policy::outlook::ExceptionKind)>,
    /// When this stall reached the ladder's second rung (`STALLED`), with the
    /// group of the call it named, when it named one: that call, if it still
    /// stands, raises the third rung one unit after it
    /// ([`super::background::BackgroundEvidence::NoProgress`]); another call
    /// is named at `STALLED` in its turn.
    stuck_told: Option<(Instant, Option<u32>)>,
    /// The canary's second flavour ([`Session::dialog_note`]): the words of the dialog no reader
    /// names standing on the screen, and when first seen. The ladder's other fields are shared
    /// with the busy flavour, which never stands with it.
    stuck_dialog: Option<(String, Instant)>,
    /// The busy flavour's words on agy's or Muse Code's screen
    /// ([`wall_memory`]'s `Witness::Words`): what stood still over their own
    /// background work, and since when.
    stuck_still: Option<(String, Instant)>,
    /// How many reprints of the same words had a far jump of the reset held
    /// back ([`turn_end_loop::REPRINT_JUMPS_MAX`]).
    reprint_jumps: u32,
    /// The act of the loop's a wall was last read to have answered (its instant,
    /// [`TurnEndState::untaken_act`]), and the wall's words at that read
    /// (`turn_end_loop::reset_key`): that read placed the wall's reset on the print, once an act
    /// and its words.
    wall_answered: Option<(Instant, String)>,
    /// The wall the loop waits on, kept while the screen
    /// shows only the agent's own background work ([`wall_memory`]).
    wall_memory: Option<wall_memory::WallMemory>,
    /// The verdict the last decision's read gave the screen's wall
    /// ([`wall_confirm`]: confirmed or refused, by the screen it was read
    /// at), and the refusal last journaled.
    wall_verdict: Option<wall_confirm::Verdict>,
    /// Identity from the accepted wall's final confirming read, by screen.
    wall_conversation: Option<(u64, super::policy::turn_end::Conversation)>,
    wall_refused_said: Option<String>,
    /// The limit a cancel left that the last decision's read took for the answer to a turn
    /// ([`TurnEndState::left_answer`]), by the screen it was read at, and whether a transcript
    /// row dated it: how that screen and its point read for the loop's books after the decision
    /// ([`Self::point_as_decided`]), and where its reset is placed ([`Self::wall_reset_at`]).
    left_read: Option<wall_confirm::LeftRead>,
    /// The id (`uuid`) of the transcript row that confirmed the screen's usage limit at the last
    /// decision's read of a wall: the notice a reply must follow for a turn to count as served
    /// since it ([`wall_confirm`]'s `served_since_notice`); `None` otherwise.
    limit_notice_row: Option<String>,
    /// The screen (its `seq`) the session's transcript last read as a turn
    /// that ended waiting on background work, with no status row drawn
    /// ([`Self::transcript_wait`], BG-1): a background break for the loop,
    /// asked of the transcript again only when the screen moves.
    transcript_wait_seq: Option<u64>,
    /// The screen (its `seq`) read busy by nothing but the wait Claude Code
    /// carries, whose turn the session's transcript last read as closed
    /// with nothing pending ([`Self::transcript_wait_over`], BG-1): an idle
    /// point, for the loop and for the policy's reading of it.
    transcript_over_seq: Option<u64>,
    /// A LIMIT ONLY THE TRANSCRIPT SHOWS: the process the host was last asked
    /// about and the row it named ([`Self::read_standing_limit`]).
    standing_read: wall_confirm::StandingRead,
    /// The last screen whose limit's words were read with the rows above
    /// ([`Self::limit_words_read_above`]) — its `seq`, `first` and rows —
    /// and the row of it the cut went through, `None` for no cut: the same
    /// screen read again takes the verdict with no request.
    limit_cut: Option<(ScreenKey, Option<usize>)>,
    /// The last screen whose last message was bounded by its cells
    /// ([`Self::message_bounded_by_cells`]) — its `seq`, `first` and rows —
    /// and the blank rows its cells made the message's own: the same screen
    /// read again folds the same rows with no request.
    message_fold: Option<(ScreenKey, Vec<usize>)>,
    /// The session's working directory as its `meta` last said it (`None`
    /// for `cwd=-` or unread; read fresh for every box that needs it).
    cwd: Option<PathBuf>,
    /// The one folder the session's starter trusted for it, as the same
    /// `meta` read said it (`trusted_cwd=`, `ApprovalCtx::trusted_cwd`).
    trusted_cwd: Option<PathBuf>,
    /// The agent a worker's spawn started on the session, as the last `meta`
    /// read said it (`worker=`, `ApprovalCtx::worker_agent`).
    worker_agent: Option<aterm_phase::Program>,
    /// The permission mode the worker's footer last showed: a box replaces the
    /// footer, so the box's own read cannot say whether the session bypasses
    /// permissions.
    footer: Option<crate::harness::lights::Mode>,
    /// A busy frame of this session was read. Its footer names any mode but
    /// the default, so after one a mode still unread ([`Self::footer`]) is
    /// no race of the agent's start, and the verdict wait is back
    /// ([`Self::agent_wake`]). Read only while no footer has been.
    busy_read: bool,
    /// The ledger of the loop in progress.
    ledger: Ledger,
    /// The turn-end policy's typing budget was seeded from the ledger
    /// ([`TurnEndState::seed_acts`]): once per session, at its first run.
    turn_end_seeded: bool,
    /// THIS LOOP IS THE SESSION'S FIRST, by the books it found where it read
    /// them ([`Self::seed_from_ledger_text`]): no row in the session's
    /// ledger, no earlier tab's ledger a cold restore noted, and its host did
    /// not start it as the session's Off lifted. Only such a loop reads a
    /// first point that holds nothing of a conversation, since a launch its
    /// host saw, as no turn's end (`Self::unasked_launch`, P3 of
    /// `docs/measured/forever-lab-2026-10-20.md`); a successor's first point
    /// is its seeds' (a relaunch's, a fault restart's, a reopened tab's), and
    /// a loop with no ledger knows of none and reads it as before.
    first_books: bool,
    /// THE NEWEST ROW OF THE BOOKS THIS LOOP FOUND (unix ms,
    /// [`approvals::last_row_ms`] over the session's ledger and the earlier
    /// tabs' a cold restore noted, as [`Self::seed_from_ledger_text`] read
    /// them): an agent process its host saw launched after it
    /// ([`IdleHost::launch`]) is one no loop before this one watched — a
    /// person who quit the agent and started it again, in a tab whose ledger
    /// holds rows — and its first point that holds nothing of a conversation
    /// is a launch's, as a first loop's is (`Self::unasked_launch`). `None`:
    /// no row, or no ledger.
    books_last_ms: Option<i64>,
    /// THE PERSON'S OWN EXCHANGES LEFT TO THEM and not closed since: the
    /// keys of the turns this loop, or an earlier one on the session or on a
    /// tab a cold restore noted, left (`approvals::open_left`; the review's
    /// RR3-TE-2). Read from the ledger at the session's first run, added to
    /// as this loop leaves one and emptied as it types
    /// ([`Self::ledger_row`]); while one names the conversation's last turn
    /// the reading says so ([`TurnEndReading::left_record`]).
    pub(super) left_keys: Vec<String>,
    /// THE LAST ANSWER OF THE SESSION'S REPOSITORY (`IdleHost::repo_work`):
    /// the mark of the done words it was asked at ([`said_mark`]), when it
    /// was asked, and what it found. The same words read again within
    /// [`REPO_REPROBE`], no turn run since, keep it
    /// (`Session::repo_read`): a point re-read at each wake asks `git` once.
    pub(super) repo_seen: Option<(
        Option<u64>,
        Instant,
        Option<crate::harness::repo_work::RepoWork>,
    )>,
    /// The screen a person's own exchange at a wall was last keyed at
    /// (`Session::ledger_left_at_wall`, RR4-TE-2): a wall the loop waits out
    /// re-decides the same screen at each wake, and its row is keyed (a
    /// transcript read) once per screen.
    pub(super) left_at_wall_seq: Option<u64>,
    /// Why the policy handed the box on the screen over, for its escalation
    /// text: the box's review key and the reason.
    box_reason: Option<(String, String)>,
    /// A fenced press that came back `reason=changed` this many times in a
    /// row: past [`MAX_CHANGED_PRESSES`] the box is handed over under the
    /// safe rules, and tried again at full power ([`Self::press_missed`]).
    changed_streak: u32,
    /// The box whose press did not land, being tried again at full power
    /// ([`approval_loop::PressRetry`]).
    press_retry: Option<approval_loop::PressRetry>,
    /// Gemini CLI's usage-limit dialog's placed reset and `Keep trying`
    /// presses since the worker last answered on another point
    /// ([`approval_loop::LimitRetry`]): the back-off between them.
    limit_retry: Option<approval_loop::LimitRetry>,
    /// The last `status` named another driver's hand on the session (a drive
    /// lease, a named driver's turn): it holds the loop as a person does.
    driver: bool,
    /// What the loop remembers of the question dialog it is answering
    /// ([`approval_loop::QuestionMemory`]): its first-key wait, its last key
    /// not yet seen taking effect. Cleared when the worker works again.
    question: approval_loop::QuestionMemory,
    /// What the loop remembers of the decline it is carrying out
    /// ([`approval_loop::DeclineMemory`]): its last keystroke not yet seen
    /// drawn, the re-raised box's pause. Cleared when the worker works again.
    decline: approval_loop::DeclineMemory,
    /// The question dialog last decided was handed to the person by the policy, a word that can
    /// change while it waits, so the wait for it re-reads the session's word
    /// ([`Self::wait_for_next`]).
    question_held: bool,
    /// The policy that held the dialog no longer does: the next look at it is
    /// a fresh decision, not the point already handed over.
    question_handed_back: bool,
    /// This loop tells the window its own story lines on its own connection
    /// (`story chose …`): the hosted loop ([`Self::run_hosted`]).
    tell_own: bool,
    /// The policy text's exact read-back ([`Self::press_question_text`])
    /// and how long the screen had held still right before it (its settle
    /// latched): the read the Enter on it is decided on
    /// ([`Step::Decide`]). Taken by the press's caller at once.
    read_back: Option<(Screen, Duration)>,
    /// The back-off after `ERR busy …`/`ERR rate`: doubles to
    /// [`REFUSAL_PAUSE_MAX`], reset by a press the server takes.
    refusal_pause: Duration,
    /// The refusal the journal last said, by its episode's key (`REFUSED <why>`, `HALTED
    /// <why>`, its measures dropped), and when it was last met: said once, again when it changes
    /// ([`Self::refusal_news`]). Over at the first write the server does not refuse
    /// ([`Self::call`]), at a hold seen lifted, and once not met for [`REFUSAL_QUIET`].
    refusal_said: Option<(String, Instant)>,
    /// Set by the hosted loop's owner: the loop ends at its next look.
    stop: Option<Arc<AtomicBool>>,
    /// A `limited:` attention a previous watcher left over the notice still
    /// showing, adopted at the start: the episode it opens is the same ask
    /// set again (`again=adopted …`, [`Session::escalate`]) — after an Off
    /// park that took the badge down too ([`Session::lower_badges`] keeps the
    /// marker). Over once the worker answers on a point that is not the
    /// notice, at a look or a read of a park ([`Session::adopted_limit_over`]):
    /// a limit after that is a new ask, and the adopted badge, if still up,
    /// goes with it.
    adopted_limit: bool,
    /// A box point's attention a previous watcher left, still showing,
    /// adopted at the start ([`Self::reconcile`]): that point's first review
    /// posts no second ask ([`Self::escalate_point`]).
    adopted_box: bool,
    /// The session's foreground program as `status program=` last named it
    /// (read for every box the policy decides and every point it acts on;
    /// `None` from a host that does not publish it).
    program: Option<String>,
    /// The session's id as that same `status` read named it (`sid=`, the
    /// instance's local id): which `who` row is the session's
    /// ([`Self::worker_env`]).
    status_sid: Option<String>,
    /// When a person last typed into the session, as that status read said (`human_ms=`;
    /// `None`: none, or a server that does not say).
    person: Option<Instant>,
    /// What that same status read said of the launch witness and the input
    /// queue ([`approval_loop::LaunchStatus`]).
    launch_status: approval_loop::LaunchStatus,
    /// The launch witness (its id) that answered a folder-trust dialog at
    /// once, and that box ([`approval_loop::box_identity`]): a witness answers
    /// one box ([`Self::launch_call`]).
    launch_used: Option<(u64, String)>,
    /// The draft in the composer as the last read showed it, and when a read
    /// first showed it so ([`Self::screen`]): a draft that changes is a
    /// person typing, whatever the server says ([`TurnEndReading::person`]).
    draft_seen: Option<(String, Instant)>,
    /// This look held an act for a person at the keyboard ([`Self::held_for_person`]: a box's
    /// answer, the survey's `0`, a stray digit's backspace): nothing escalated, the point looked
    /// at again at [`Self::turn_end_due`].
    held: bool,
    /// The deadline of a held act ran out: the next look judges the point
    /// on the screen again, as a new one.
    rejudge: bool,
    /// The point showing was left to the host: decided while it owned turn ends
    /// ([`IdleHost::owns_turn_end`]), or passed over for a step that typed and ended nothing
    /// ([`Self::host_steps_here`]). Once the host owns nothing the point is decided again
    /// ([`Self::wait_for_next`]), so it never sits.
    host_held: bool,
    /// THE POINT THE HOST'S STEP LEFT STANDING WHILE IT STILL WANTED ONE
    /// (F1-R6-A1, F1-R7-B1): the review key of an idle point where its step
    /// ([`IdleHost::at_idle`]) moved nothing ([`HostStep::moved`]: it took
    /// nothing, or a wait, a give-up) and it asked for a point again all the
    /// same ([`IdleHost::wants`]). That point is not offered it again until a
    /// step of the wait has run ([`Self::wait_for_next`]); another point is.
    /// Offered at once, the same point went back to the host as fast as a
    /// look could read it: thousands of times a second on a server that
    /// answers at once, twice a second (each look's `await idle 500`) on the
    /// real one — a Codex nobody had prompted yet (a step that took
    /// nothing), a Codex lane's `wait:goal-held` for up to an hour while its
    /// paused goal was held (a step that took a wait), each a whole upgrade
    /// step and a journal row. The one exception is [`Self::host_follows`].
    host_passed: Option<String>,
    /// THE POINT OFFERED AGAIN AT ONCE FOR A FOLLOW-UP (F1-R7-B1, F1-R8-L1):
    /// the review key of an idle point where a step took something that
    /// moved nothing while the host owned no turn end, and still wanted a
    /// point — a give-up, whose release is owed at the next idle point — and
    /// how many steps there have had it back so. Once the host owns nothing
    /// the wait decides the point again at once ([`Self::host_held`]), so
    /// that point goes back to the host first: its release before the
    /// policy's `keep going`, never both. AT MOST [`HOST_FOLLOW_UPS`] TIMES:
    /// one more step there that again moves nothing passes the point over
    /// ([`Self::host_passed`]), so no word of that shape loops either.
    /// Counted, not once (F1-R8-L1): a wait whose step a race on the host
    /// thread left wanting a point (an activation notice, the upgrade
    /// watch, a look turned into a park) has the follow-up's shape too, and
    /// spent the once — the give-up offered at once after it passed the
    /// point over, and the policy typed `keep going` before its release.
    /// THE POINT'S COUNT (F1-R9-L1): a step there that moved the session
    /// spends none of it and keeps it, so steps that move nothing with such
    /// a word between them still pass the point over at the count — at most
    /// `2 * (HOST_FOLLOW_UPS + 1)` steps between two steps of the wait;
    /// reset on that word, they went back to the host at once to its cap.
    /// The third field: the last step there moved the session — a move
    /// after it at the same point spends one (a word that
    /// only ever moves, said at a point the screen never left).
    host_follows: Option<(String, u8, bool)>,
    /// The box on the screen (its review key) and when a read first showed
    /// it: judged only once it has shown the setup's box settle ([`BOX_SETTLE`]).
    box_first_seen: Option<(String, Instant)>,
    /// The autocompact offer's stepper on the offer now up
    /// ([`approval_loop::StepperTrack`]).
    stepper: approval_loop::StepperTrack,
    /// The remembered transcript candidate, without current-mount identity;
    /// a later screen mount spends it ([`Self::box_call`]).
    box_proved: Option<String>,
    /// The rm breaker's too-many box: the transcript's one waiting Bash call
    /// and when a read on this box's mount first found it
    /// ([`Self::box_command`]). The settle runs from that sighting, so the
    /// box decided again (a ticking countdown moved the screen past the
    /// judged read and the press was skipped) takes the same call without
    /// waiting the settle again. A later mount forgets it
    /// ([`Self::spend_proved`]).
    box_seen: Option<(super::policy::box_command::BoxCommand, Instant)>,
    /// Spent transcript candidates, newest last (at most [`SPENT_CALLS`]).
    /// Excluding a remembered candidate after a screen change avoids reuse;
    /// it does not bind any call to the current box.
    calls_spent: Vec<String>,
    /// The point (its review key) an `await agent` last woke the loop from
    /// ([`Self::agent_wake`]): the same point read again means the server's
    /// verdict and the loop's reading disagree there, and it is waited on by
    /// its content instead.
    agent_woke: Option<String>,
    /// The composer that holds the point last decided where nothing is due
    /// ([`Self::draft_look`]): aterm's own words or a draft over a completion
    /// menu (escalated for itself, not for a question or the cap), a draft
    /// whose cells were not read (nothing done), or any draft the owner's
    /// notice names as all that holds the tab, whatever the point waits on
    /// (`policy::notice::DRAFT_HOLDS`). A person's send or clear
    /// there moves no agent verdict — a turn shorter
    /// than the server's sweep is never read busy — so while it is set the
    /// content is waited on ([`Self::agent_wake`]), and the point is decided
    /// again once its composer reads otherwise.
    draft_held: Option<super::policy::turn_end::Composer>,
    /// Where the loop stands on the session's supervisor claim
    /// ([`claim::Claim`]).
    claim: claim::Claim,
    /// What the claim's renewals changed, for the next look to say.
    claim_said: Vec<String>,
    /// The claim was taken at naming ([`Self::claim_at_naming`]), before the
    /// loop's first read: the start's take is said, not asked again.
    claimed_at_naming: bool,
    /// The text of this loop's own keyed attention entry (owner
    /// [`ATTENTION_OWNER`]), as last set and not yet unset.
    attention_ours: Option<String>,
    /// The turn-end policy raised a badge that no point carries (`/login`
    /// typed, "finish sign-in in the browser"): cleared once the worker
    /// works again.
    turn_end_badge: bool,
    /// That badge's text, taken down as an Off park starts ([`Session::lower_badges`]) and set
    /// again as the park ends on a word ([`Session::raise_parked_badge`]); forgotten where a read
    /// of the park sees the worker working, which ends that badge.
    badge_parked: Option<String>,
    /// The exception the policy found at the point last decided
    /// ([`policy::exception::exception_for`]), set by
    /// [`Self::decide_to_act`] and told at the point's execution
    /// ([`Self::sync_exception`]).
    exception_now: Option<Exception>,
    /// The exception told to the journal and the host and not yet ended, and
    /// since when ([`Self::sync_exception`], [`Self::exception_ended`]).
    exception_open: Option<(Exception, Instant)>,
    /// The turn-end policy's task-done state as the journal last said it
    /// ([`super::policy::turn_end::TurnEndState::task_done`]): `DONE …` is
    /// journaled once as the task ends, not at every point after.
    task_done_said: bool,
    /// The wall time (unix ms) the landing the journal last said was first
    /// noted ([`Self::task_edge`]): what a note restating it after the task's
    /// own wake carries ([`super::policy::turn_end::TurnEndState::take_wake_restated`]).
    landed_noted_ms: Option<i64>,
    /// The task's end said at the point's fold ([`Self::task_edge_at_fold`])
    /// and owed to the journal: written right after the point's own line is
    /// ([`Self::say_task_note`]), so a loop that follows reads the point
    /// before its note (`ended_before`), with the time the end it restates
    /// was first noted, where it restates one
    /// ([`TurnEndState::restated_noted_ms`]).
    task_note: Option<(String, Option<i64>)>,
    /// The point ([`TurnEndState::point_number`]) whose `done` this loop
    /// last published to the session's task record, or found published
    /// (`task_mark`): once a point.
    task_point: Option<u64>,
    /// THE WORDS THE LAST FOLDED POINT'S SCREEN SAID, by its program's reader
    /// ([`super::policy::turn_end::TurnEndReading::turn_said`] and
    /// `said_tail`, whole: none where they were cut): a done's result where
    /// the session's harness has no transcript aterm reads (agy, Muse Code,
    /// Gemini CLI; `task_mark`'s `publish_done`).
    point_said: Option<(aterm_phase::Program, String)>,
    /// THE WORKER'S REPORT A SCREEN-READ LANDING'S RESULT OPENS WITH: the
    /// [`Self::point_said`] of the point this loop typed the Land episode's
    /// instruction at — the turn before its land acts, as a transcript's
    /// result reads it (`harness::result`'s LANDING rule) — kept until the
    /// episode ends ([`Self::land_over`]). `None`: no episode this loop
    /// began, or that point's words were cut.
    land_report: Option<(aterm_phase::Program, String)>,
    /// The session's task record was read at this loop's first fold
    /// (`task_mark`'s `task_seed`): read once a loop.
    task_seeded: bool,
    /// The key of the NEEDS YOU set this loop put on the person's screen
    /// ([`super::policy::turn_end::TurnEndState::needs_key`],
    /// [`NEEDS_ATTENTION_OWNER`]'s entry; `escalate.rs`'s `sync_needs`):
    /// raised again only for another set, taken down once none stands.
    /// `Some(0)` (`escalate.rs`'s `NEEDS_LEFT`): an entry a loop before this
    /// one left, found at this loop's start (`lifecycle.rs`'s `reconcile`)
    /// and settled at its first point.
    needs_told: Option<u64>,
    /// The text [`Self::needs_told`]'s set was put up with, as the server
    /// took it (cut, where a cap's refusal cut it); empty while no set of
    /// this loop's stands. What the window's host is told again where a
    /// request the connection lost left it told otherwise than the entry
    /// this loop holds up (`escalate.rs`'s `sync_needs`). Reporting only:
    /// nothing the loop decides reads it.
    needs_told_text: String,
    /// What the window's host was last told of the NEEDS YOU set
    /// ([`IdleHost::needs_standing`], `escalate.rs`'s `tell_needs`): the
    /// set's key and the entry's text it was told with (`(0, "")`: the
    /// entry coming down, or this loop's words taken back where the entry
    /// stopped being its own, [`Self::needs_not_ours`]), so the host hears
    /// a set again only where the set or that text moved — never again for
    /// a request the connection lost and the next point sends once more.
    /// `None`: told nothing this loop. Reporting only: nothing the loop
    /// decides reads it.
    needs_heard: Option<(u64, String)>,
    /// The host's measure of the API's reach when the point on screen is a network wall
    /// ([`Self::turn_end_reading`]); `None` otherwise. While `Some` and a policy wait stands,
    /// each wait step asks again, and a new measure decides the point again ([`Self::reach_edge`]).
    reach_seen: Option<Reach>,
    /// What the host advised about the session's account at the point being decided
    /// ([`IdleHost::account`]), with the screen and the point it was asked for: the answer for
    /// every reading of that point within one decision. Forgotten where a decision begins
    /// ([`Self::account_round`]).
    account_seen: Option<(u64, AccountPoint, Option<AccountAdvice>)>,
    /// What the host said of the session's login at the weekly usage limit
    /// being decided ([`IdleHost::roster_word`]), with the screen it was
    /// asked for: kept and forgotten as [`Self::account_seen`] is, and like
    /// it, a decision that read it hears the accounts' news while its wait
    /// stands ([`Self::account_edge`]).
    roster_seen: Option<(u64, Option<super::policy::turn_end::RosterWord>)>,
    /// The point decided last was carried out as NOTHING
    /// ([`Self::turn_end_execute`]; forgotten where a decision begins,
    /// [`Self::account_round`]): no wait set, nothing typed, nothing
    /// escalated — so deciding it again posts nothing twice, and a decision
    /// that read the roster's word hears the accounts' news with no wait
    /// standing ([`Self::account_edge`]).
    nothing_decided: bool,
    /// The point decided last was NOTHING because the host withheld the turn
    /// ends ([`IdleHost::withholds_turn_end`], read by the decision): its
    /// HOLD's `why=withheld` ([`HoldSaid::Point`]), which so asks the host
    /// nothing more than the decision did. Taken by that point's act
    /// ([`Self::turn_end_execute`]), so a point decided with no read of the
    /// host says none.
    decided_withheld: bool,
    /// The day count of Claude Code's expiry notice this loop last told its host
    /// ([`IdleHost::login_notice`]): each value is told once.
    login_notice_told: Option<u32>,
    /// The host's stamp ([`IdleHost::account_stamp`]) as this session last read it: one moved
    /// since is the accounts' news ([`Self::account_news`]). `None` until the first read, which
    /// is no news.
    account_stamp: Option<u64>,
    /// When the accounts' news last ended a latched session's wait ([`Self::account_edge`]): the
    /// next is heard no sooner than [`ACCOUNT_NEWS_EVERY`] after it.
    account_news_at: Option<Instant>,
    /// The host's stamp ([`IdleHost::account_stamp`]) read just before the host said the process
    /// at the lost login on screen holds its renewable row's own login
    /// ([`IdleHost::login_superseded`] `false`, at a decision's read, [`Self::turn_end_reading`]):
    /// the point decided on that reading asks the owner for the renewal. `None` at every other
    /// decision's read. A wait step whose stamp has moved past it asks the host again
    /// ([`Self::renewal_made`]).
    login_seen: Option<u64>,
    /// The accounts probe's panel, as this loop remembers it
    /// ([`UsageProbe`]): `Idle` for the whole life of every loop on a
    /// machine with no roster, where no probe is claimed and the session
    /// ledger holds no row of one.
    probe: UsageProbe,
    /// The grid (`cols`, `pane_rows`) at which a probe read only the fold ([`ProbeMiss::Fold`]):
    /// this loop does not probe that size again, or a short window would be typed into every
    /// three minutes for nothing (the spec's P11). A resize is another size.
    probe_fold: Option<(Option<u16>, Option<usize>)>,
    /// This loop probes no more: what came up was not the Usage tab under
    /// the fullscreen renderer's rule, or the point's key changed across a
    /// probe (the spec's P21) — something the step left on the screen.
    probe_off: bool,
    /// The look in progress took its point after a read of the worker's own busy row
    /// ([`Saw::live`]): its last response is newer than any usage snapshot the home holds (the
    /// spec's P7).
    turn_live: bool,
    /// The point ([`Session::review_key`]) the loop last spoke for about its probe (a gate shut
    /// on a due one, [`Session::probe_shut`], or a claim): the same point decided again says
    /// nothing more. Forgotten when a look takes its point after a busy read.
    probe_said: Option<String>,
    /// Until when the owed panel's Escape is left to a person's hand
    /// ([`Session::usage_panel_look`]): the `HELD` line is said once a hold.
    probe_hold: Option<Instant>,
    /// The limit reset's step, as this loop remembers it ([`ResetStep`]):
    /// `Idle` for the whole life of every loop whose ledger holds no open
    /// row of the rule and that takes none, and of every attended loop.
    reset: ResetStep,
    /// The host's go for the grant row of the dialog now up, and when it was
    /// first asked for: from its `Take` until the row's Enter is written
    /// ([`ResetStep::Owed`] from there) or the reset is passed over. A claim
    /// that stands with no row taken is told kept when the dialog is left
    /// ([`Session::reset_untaken`]).
    reset_taken: Option<(ResetGo, Instant)>,
    /// The kinds of reset the grant row was chosen under
    /// (`[harness] limit_reset_kinds` as [`Session::reset_take`] read it):
    /// what the row's press reads the dialog again by
    /// ([`Session::press_focused`]), so the row it enters is still the one
    /// the rule takes.
    reset_kinds: Vec<aterm_phase::ResetKind>,
    /// The dialog mount (its [`Self::box_first_seen`] key) the reset was passed over for (a gate
    /// shut, the host said no, the row's press never landed): the rest of that mount is the wait
    /// row's and its host is not asked again. Forgotten with the mount ([`Session::box_young`]).
    reset_passed: Option<String>,
    /// How many times in a row the host answered that it cannot say yet
    /// ([`ResetWord::Again`]) at this mount ([`reset_loop::RESET_AGAIN_MAX`]).
    reset_again: u8,
    /// What the usage-limit dialog is answered with where its reset is passed over: the decision
    /// made without the reset's rule, on the same read ([`Session::decide_box`]).
    reset_fallback: Option<Decision>,
    /// What the last box was decided on ([`Session::decide_box`]): the facts
    /// its context was built from alone, which the decision's mark on a
    /// harness tape carries for diagnostic re-decision.
    /// `None` for a box the loop handed over before the policy was asked.
    decided_on: Option<super::policy::box_facts::BoxFacts>,
    /// Whether the last focus press ([`Session::press_focused`]) reached its
    /// answering key: a press lost before it wrote arrows at most, which
    /// choose nothing.
    focus_key_tried: bool,
    /// `status` last said the session is halted (`hold=1`,
    /// [`Session::foreground_program`]).
    halted: bool,
    /// The session's `autopilot` word as the last bare `meta` named it
    /// ([`approval_loop::AutopilotWord`]): the turn-end policy's mode, and at `off` the loop's
    /// park ([`Session::park_while_off`]).
    autopilot: approval_loop::AutopilotWord,
    /// The local zone's offset at an instant ([`limit::local_offset_at`]; a
    /// table in tests): what a day the vendor draws with no zone is placed by
    /// ([`Session::reset_place`]), read only through
    /// [`Self::local_offset_then`] — `probe.local_offset_at` on a tape.
    local_offset_at: fn(i64) -> Option<i64>,
    /// The host's memory measure acted on ([`super::memory::MemoryOver::acts`]) as the last
    /// turn-end reading ([`Self::turn_end_reading`]) judged it; `None` for a warning only, under
    /// the ceiling, or no measure. An idle point decided without one asks again each wait step.
    memory_seen: Option<super::memory::MemoryOver>,
    /// The last turn-end reading took [`Self::memory_seen`] as its wall
    /// ([`Self::turn_end_reading`]: an idle point, a wall's own included):
    /// an escalation decided there is the memory's, raised by the point even
    /// at a limit notice ([`Self::escalates_here`]).
    memory_wall: bool,
    /// The last turn-end reading took the memory wall in place of a screen wall (a limit, a lost
    /// login, an API error waited out): a measure back under the ceiling is then an edge too
    /// ([`Self::memory_edge`]), and the point is decided again by the wall's own rule.
    memory_over_wall: bool,
    /// The memory warning was said ([`Self::memory_warn`]) with this verdict:
    /// once an episode (which ends a measure an eighth under the ceiling,
    /// [`super::memory::episode_over`]), and again whenever the verdict changes.
    memory_told: Option<super::memory::Verdict>,
    /// The host's last word on a memory restart was a refusal (`restart_agent`'s `otherwise`),
    /// so the warning's verdict is the person's restart ([`super::memory::Verdict::Person`]).
    /// Cleared only by a restart `restart_agent` sees made; not by a `wait:`, the agent working,
    /// a measure no longer acted on, or the warning episode's end: a refusal is mostly the agent's
    /// own launch (`refused:one-shot`, `refused:self`), which stands for its life.
    memory_refused: bool,
    /// BESIDE A PROVEN SELF-CLAIM, the agent process the memory ceiling's
    /// restart was last made (or refused for good) on
    /// ([`Self::beside_memory_remedy`]): one act per process, and none again
    /// until the measure is no longer acted on.
    beside_memory_ended: Option<u32>,
    /// The process, content sequence and first eligible idle sample for the
    /// memory remedy's observation window. Any ineligibility starts over.
    beside_idle_at: Option<(u32, u64, Instant)>,
    /// The wait last told to the host ([`IdleHost::waiting`]): `Some(None)` a told `None`, `None`
    /// nothing told yet. Told again only when its deadline moves or its told unix second drifts
    /// past [`WAIT_SKEW_S`].
    wait_told: Option<Option<ToldWait>>,
    /// A test gave this session state its loop LEARNS as it runs (a look
    /// taken, a turn's span begun at a raw `Instant`), not state a setup can
    /// hold: why, if so. Its run cannot be replayed from a tape — the replay
    /// rebuilds the loop from its setup alone — and the whole-run helpers say
    /// so instead of replaying it ([`Self::seeds_learned_state`]).
    #[cfg(test)]
    pub(crate) unreplayable: Option<&'static str>,
    /// The stall `status input=stalled|stopped` reported and this loop holds
    /// on ([`stall`]): nothing is pressed, typed or escalated until it lifts.
    stalled: Option<stall::WorkerStall>,
    /// The host told of the stall held and lifted ([`IdleHost::stalled`]) and of what each read
    /// says of the account's usage ([`IdleHost::usage_seen`]): the loop's own, set as it starts.
    stall_host: Option<Arc<dyn IdleHost>>,
    /// The session log's `act` marks this loop owes ([`ActMarks`]).
    pub(super) act_marks: ActMarks,
    /// What the host was last told of the account's usage, and when ([`Self::note_usage`]).
    usage_told: Option<(UsageSeen, Instant)>,
    /// WHAT THE NOTICES THAT SAY USAGE CREDITS CARRY THE SESSION SAID
    /// ([`Self::note_credits`]): the reset the notice they started on named
    /// (`You're now using usage credits · Your session limit resets 9:43am
    /// (…)`), as printed and placed (epoch seconds; `None`: it would not
    /// place) — drawn some ten seconds as they begin, mostly under the running
    /// turn — the model a per-model notice named (`Fable 5.1 is drawing from
    /// usage credits`, drawn once a process), and the first read of either:
    /// what the turn-end reading's [`super::policy::turn_end::CreditsCarry`]
    /// is at a point the notice has left.
    pub(super) credits_kept: Option<CreditsKept>,
    /// The guard last said to the host ([`IdleHost::withheld`]), so each is
    /// said once while it stands: `None` once a point was offered, or while
    /// the host asked for none.
    withheld: Option<Guard>,
    /// The hold this loop last journaled ([`Self::say_hold`]), so each is
    /// said once while it stands: `None` once the agent works again (a busy
    /// read that is no break), a break was offered, or a point decided
    /// anything but NOTHING. Never seeded: a successor says its first again.
    hold_said: Option<HoldSaid>,
    /// THE OCCUPANCY HOLD'S CLOCK ([`occupancy_loop`]): how long the agent has waited on its
    /// own background work, journaled (`OCCUPANCY`) and booked (`fact-hold@v1`) on a hosted,
    /// unattended loop while `[harness] occupancy` is not `off`, and carried by a successor from
    /// its ledger row. Types nothing.
    hold: occupancy_loop::HoldWatch,
    /// A press was refused `ERR busy input-unread` or written with its
    /// `warn=input-unread` ([`CtlReply::warned_input_unread`]), or a `status`
    /// read for another reason reported a stall not held yet
    /// ([`Self::note_status`]): nothing is pressed, typed or escalated until
    /// `status` is read again ([`Self::stall_look`], [`Self::stall_step`]).
    stall_suspect: bool,
    /// This episode's stall has had its remedy, the explicit `restart` or its warning
    /// ([`Self::stall_remedy`]): never a second. Forgotten with the episode.
    stall_termed: bool,
    /// `[harness] human_grace_s`, for the remedy's own look at a person's
    /// hand: the loop's policy, set as it starts.
    stall_grace_s: u32,
    /// `[harness] stall_term_after_s`, the remedy's bound (`0`: never): the
    /// loop's policy, set as it starts.
    stall_term_after_s: u32,
    /// `[harness] resume_limits`: the turn-end policy continues a limit at its
    /// reset ([`Self::waits_out`]). The loop's policy, set as it starts.
    resume_limits: bool,
    /// `[harness] stall_term`: the remedy at the bound is the explicit restart, `restart
    /// pid=<leader> if-stalled quiet=<grace>` (`signal term` on an older server), else a warning
    /// with the agent left running ([`stall`]). Set as the loop starts.
    stall_term: bool,
}

/// The approval policy's inputs from the supervisor's own process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalEnv {
    pub home: Option<PathBuf>,
    pub uid: u32,
    pub tmpdir: Option<PathBuf>,
    /// Where the worker's environment comes from: the environment a git read's configuration is
    /// read with (`policy::git_config`), and whose `CLAUDE_CONFIG_DIR` (else `$HOME/.claude`)
    /// holds the session files and transcripts that say where its Bash tool stands
    /// ([`crate::harness::footer::shell_cwds`]).
    pub worker: WorkerSource,
}

/// Where [`ApprovalEnv::worker`] comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerSource {
    /// The session's own foreground process, read for every box that may run
    /// git ([`Session::worker_env`]).
    Session,
    /// This environment, as though read from the session — `Err` as though it
    /// could not be (a test's).
    Fixed(Result<super::policy::WorkerEnv, String>),
}

impl ApprovalEnv {
    /// This process's `$HOME`, real uid and `$TMPDIR`, and the worker read
    /// from the session ([`WorkerSource::Session`]) — in this crate's own
    /// unit-test build a hermetic stand-in with no Claude Code directory, so
    /// no test's verdict depends on the developer's git config or live
    /// Claude Code sessions.
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
    /// The session `sid` (`None`: the caller's own) under the shipped setup
    /// ([`SessionSetup::new`]); the setters below change what it was given.
    pub fn new(ctl: &'a mut C, sid: Option<String>) -> Self {
        Self::from_setup(ctl, SessionSetup::new(sid))
    }

    /// The session a setup describes: what [`Self::new`] and the setters
    /// make, given whole — the form a tape's header comes back in, so a
    /// replay's loop is told exactly what the recorded one was, and nothing
    /// is read from this process for it. Everything else starts as a new
    /// loop's does.
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
            stepper: approval_loop::StepperTrack::default(),
            box_proved: None,
            box_seen: None,
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

    /// The holder an unattended loop claims the session under (`meta set
    /// supervisor <holder>`); unset, `aterm-supervise:<pid>`. The in-GUI
    /// host names itself, so another holder's live claim on the same session
    /// is told from its own.
    pub fn set_supervisor_name(&mut self, name: Option<String>) {
        self.setup.holder = name.unwrap_or_else(claim::default_holder);
    }

    /// Whether the loop types its texts — a continuation, the done check, an
    /// answer, the worker's own suggestion (its words, never the accept
    /// key), the accounts probe's `/usage` — under this process's hand
    /// (`crate::harness::upgrade_drive::hand_take_here_request`): the
    /// window's host, whose one connection holds the lease from the text to
    /// its Enter, so no other connection types between them and the ledger
    /// reads the turn as the harness's (`history … by=harness`). Off for a
    /// loop whose client may open a connection per request. The setup's
    /// ([`SessionSetup`]), so a tape's replay types as the run did.
    pub fn type_under_hand(&mut self, on: bool) {
        self.setup.types_under_hand = on;
    }

    /// Watch beside a proven self-claim: the agent supervises itself under `holder`'s claim, so
    /// this loop takes no claim of its own and watches only (every write it would make refused
    /// locally, as behind any other holder), yet keeps aterm's remedies that act from outside the
    /// agent: the stall's explicit restart and the memory ceiling's measure
    /// ([`super::claim::Claim::Beside`]). `None`: claim as usual.
    pub fn set_beside(&mut self, holder: Option<String>) {
        if self.setup.beside != holder {
            self.beside_idle_at = None;
        }
        self.setup.beside = holder;
    }

    /// The hosted loop's hand-over flag (the setup's, [`SessionSetup`]): the host sets
    /// it before the stop when a new loop takes the session straight after.
    pub fn set_handover(&mut self, flag: Arc<AtomicBool>) {
        self.setup.handover = setup::Handover(Some(flag));
    }

    /// Tests: the claim's renewal step and lease.
    #[cfg(test)]
    pub(crate) fn set_claim_timing(&mut self, renew: Duration, ttl_ms: u64) {
        self.setup.claim_renew = renew;
        self.setup.claim_ttl_ms = ttl_ms;
    }

    /// Keep the approval ledger at `path` ([`approvals::default_path`] for
    /// the CLI and the host); `None` keeps none.
    pub fn set_approval_ledger(&mut self, path: Option<PathBuf>) {
        self.setup.ledger_path = path;
    }

    /// The approval ledger this loop keeps, if any
    /// ([`Self::set_approval_ledger`], or a hosted loop's `opts.ledger`).
    #[must_use]
    pub fn approval_ledger(&self) -> Option<&Path> {
        self.setup.ledger_path.as_deref()
    }

    /// What this session was given before its loop runs, as the setters left
    /// it: what a tape's header writes down ([`SessionSetup`]).
    #[must_use]
    pub fn setup(&self) -> &SessionSetup {
        &self.setup
    }

    /// Tests: the approval policy's process inputs.
    #[cfg(test)]
    pub(crate) fn set_approval_env(&mut self, env: ApprovalEnv) {
        self.setup.approval_env = env;
    }

    /// Tests: a fixed clock (Unix seconds) and local zone offset, so a
    /// notice's reset lands where the test says.
    #[cfg(test)]
    pub(crate) fn set_clock(&mut self, now: i64, local_offset_s: i64) {
        self.set_clock_at_ms(now.saturating_mul(1000), local_offset_s);
    }

    /// Tests: [`Self::set_clock`] to the millisecond, so a test can stand the
    /// clock just short of a second's turn.
    #[cfg(test)]
    pub(crate) fn set_clock_at_ms(&mut self, now_ms: i64, local_offset_s: i64) {
        self.test_clock().set_at_ms(now_ms);
        self.local_offset_fixed = Some(local_offset_s);
        self.local_offset = None;
        // The fixed offset holds on every day ([`Self::set_local_lookup`]).
        self.local_offset_at = |_| None;
    }

    /// Tests: the local zone's offset at an instant, in place of `date`.
    #[cfg(test)]
    pub(crate) fn set_local_lookup(&mut self, lookup: fn(i64) -> Option<i64>) {
        self.local_offset_at = lookup;
    }

    /// Tests: the fixed clock jumps `jump` seconds ahead after its next read
    /// — the wall clock of a Mac that sleeps between two steps of the loop's
    /// wait, while the monotonic clock stands still. Once: the Mac wakes.
    #[cfg(test)]
    pub(crate) fn set_clock_jump(&mut self, jump: i64) {
        self.test_clock().set_jump(jump);
    }

    /// Tests: the machine counts its sleep from now, the count at `total`
    /// ([`super::fx::ScriptedClock::count_sleeps`]): a sleep the fixed clock
    /// stages is counted, and a step of it is time awake.
    #[cfg(test)]
    pub(crate) fn count_sleeps(&mut self, total: Duration) {
        self.test_clock().count_sleeps(total);
    }

    /// Tests: the machine slept `secs` up to now, the fixed clock already
    /// past it ([`super::fx::ScriptedClock::count_slept`]).
    #[cfg(test)]
    pub(crate) fn count_slept(&mut self, secs: i64) {
        self.test_clock().count_slept(secs);
    }

    /// Tests: the boot the machine's count is on
    /// ([`super::fx::ScriptedClock::set_boot`]).
    #[cfg(test)]
    pub(crate) fn set_boot(&mut self, boot: [u8; 16]) {
        self.test_clock().set_boot(boot);
    }

    /// Tests: the machine's record of its last sleep
    /// ([`super::fx::ScriptedClock::set_last_sleep`]).
    #[cfg(test)]
    pub(crate) fn set_last_sleep(&mut self, last: Option<(i64, i64)>) {
        self.test_clock().set_last_sleep(last);
    }

    /// Tests: the fixed clock stepped `jump` seconds ahead after its next
    /// read, no sleep ([`super::fx::ScriptedClock::set_step`]).
    #[cfg(test)]
    pub(crate) fn set_clock_step(&mut self, jump: i64) {
        self.test_clock().set_step(jump);
    }

    /// Tests: the sleeps this loop knows of ([`Self::sleeps`]).
    #[cfg(test)]
    pub(crate) fn sleep_book(&self) -> &SleepBook {
        &self.sleeps
    }

    /// Tests: a hand on the fixed clock for a stand-in host, to jump it
    /// forward from inside the loop's run ([`ClockJump::slept`]: a sleep
    /// staged at a step of the host's).
    #[cfg(test)]
    pub(crate) fn clock_jump_handle(&mut self) -> ClockJump {
        ClockJump(
            self.fx
                .scripted_handle()
                .expect("a fixed clock: the session's door reads no virtual clock"),
        )
    }

    /// Tests: the fixed clock runs from now, as the system's does. A loop that waits in real
    /// `Instant`s for a reset a few seconds off converts the reset from unix time at every
    /// reading ([`Self::wall_reset_at`]); a clock standing still would slide that target ahead
    /// for ever.
    #[cfg(test)]
    pub(crate) fn set_clock_running(&mut self) {
        self.test_clock().set_running();
    }

    /// Tests: the monotonic clock STANDS from now beside the fixed wall
    /// clock ([`super::fx::ScriptedClock::stand_mono`]), for a test that ages
    /// the policy's instants itself at each step.
    #[cfg(test)]
    pub(crate) fn set_mono_standing(&mut self) {
        self.test_clock().stand_mono();
    }

    /// Tests: the fixed clock the setters above stage ([`super::fx::ScriptedClock`]),
    /// the session's door made one first if it read the system's.
    #[cfg(test)]
    fn test_clock(&mut self) -> &super::fx::ScriptedClock {
        self.fx
            .scripted()
            .expect("a fixed clock is staged over the system's, never over a virtual one")
    }

    /// The loop's door to its world ([`super::fx::Fx`]): a recording one for a
    /// run made with `--tape` (`drive_cli`); in tests, e.g. one over virtual
    /// clocks that only sleeps advance.
    pub(crate) fn set_fx(&mut self, fx: Fx) {
        self.fx = fx;
    }

    /// Tests: a hold for a person ([`Self::held_for_person`]) is looked at again after `wait`,
    /// not after the rest of their grace; the hold is still decided on the whole grace.
    #[cfg(test)]
    pub(crate) fn set_person_hold_wait(&mut self, wait: Duration) {
        self.setup.person_hold_wait = Some(wait);
    }

    /// Tests: the wake's settle has passed — it, and the due the Mac's wake
    /// placed at it ([`Self::woke`]), fall now — with none of it waited out
    /// in real time.
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

    /// Tests: this test seeds state the loop learns as it runs, not state its
    /// setup holds (`why`): its run is not replayed from its tape.
    #[cfg(test)]
    pub(crate) fn seeds_learned_state(&mut self, why: &'static str) {
        self.unreplayable = Some(why);
    }

    /// Tests: a zone table in place of `date`.
    #[cfg(test)]
    pub(crate) fn set_zone_lookup(&mut self, lookup: fn(&str, i64) -> Option<i64>) {
        self.zone_offset = lookup;
    }

    /// The loop's own bounded waits ([`crate::harness::Timings`]; tests run
    /// [`crate::harness::Timings::FAST`], the host this build's).
    pub fn set_timings(&mut self, timings: crate::harness::Timings) {
        self.setup.claim_ttl_ms = claim::ttl_ms(&timings);
        self.setup.redecide_after_skip = timings.redecide_after_skip;
    }

    /// The turn-end policy's timings (tests shorten them; the host keeps
    /// the defaults). Starts the policy's memory of the session afresh.
    pub fn set_turn_end_timing(&mut self, timing: TurnEndTiming) {
        self.turn_end = TurnEndState::new(timing.clone());
        self.setup.turn_end_timing = timing;
    }

    /// How long an outage is ridden out before the loop ends
    /// (`--reconnect-s`; 180 s unless set; zero: not at all).
    pub fn set_reconnect(&mut self, window: Duration) {
        self.setup.reconnect = window;
    }

    /// How long presses that miss are tried before the badge, shortened for
    /// a test.
    #[cfg(test)]
    pub(crate) fn set_press_badge_after(&mut self, after: Duration) {
        self.setup.press_badge_after = after;
    }

    /// The survey's retry pauses, shortened for a test.
    #[cfg(test)]
    pub(crate) fn set_survey_retry(&mut self, pauses: [Duration; SURVEY_TRIES]) {
        self.setup.survey_retry = pauses;
    }

    /// How long a rating card must have shown before its `0` goes ([`SURVEY_SETTLE`]),
    /// shortened for a test (a scripted server has no digit guard).
    #[cfg(test)]
    pub(crate) fn set_survey_settle(&mut self, settle: Duration) {
        self.setup.survey_settle = settle;
    }

    /// How long the harness's `0` is given to be taken before a lone `0` under the open
    /// card is called dropped ([`SURVEY_LAND`]), set for a test: a scripted server has no
    /// clock, so a test picks the world (zero: the vendor's debounce has run).
    #[cfg(test)]
    pub(crate) fn set_survey_land(&mut self, land: Duration) {
        self.setup.survey_land = land;
    }

    /// The accounts probe's bounds ([`PROBE_WAITS`]), shortened for a test:
    /// how long the panel is waited for, read for, paused between two reads
    /// of, and given to leave after its Escape.
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

    /// The limit reset's bounds ([`reset_loop::RESET_WAITS`]), shortened for
    /// a test.
    #[cfg(test)]
    pub(crate) fn set_reset_waits(&mut self, waits: reset_loop::ResetWaits) {
        self.setup.reset_waits = waits;
    }

    /// How long a box must have shown before it is pressed ([`BOX_SETTLE`]),
    /// shortened for a test.
    #[cfg(test)]
    pub(crate) fn set_box_settle(&mut self, settle: Duration) {
        self.setup.box_settle = settle;
    }

    /// How long apart the transcript is read again for a box's call
    /// ([`super::policy::box_call::LAG_STEP`]), shortened for a test.
    #[cfg(test)]
    pub(crate) fn set_box_call_step(&mut self, step: Duration) {
        self.setup.box_call_step = step;
    }

    /// How long the autocompact offer must have shown before its first key
    /// ([`AUTOCOMPACT_SETTLE`]), shortened for a test.
    #[cfg(test)]
    pub(crate) fn set_autocompact_settle(&mut self, settle: Duration) {
        self.setup.autocompact_settle = settle;
    }

    /// How long the agent's background work must have stood before its host
    /// is offered the break ([`background_settle`]), shortened for a test.
    #[cfg(test)]
    pub(crate) fn set_background_settle(&mut self, settle: Duration) {
        self.setup.background_settle = settle;
    }

    /// The features the server turned out to have (for diagnostics).
    #[cfg(test)]
    pub(crate) fn caps(&self) -> Caps {
        self.caps
    }

    /// One request to the target session; a client that could not be
    /// launched at all is final. Keeps the outage's books: a request the
    /// server did not serve ([`Self::unserved`]) starts one, unless one is in
    /// progress; a request of the outage's kind served again ends it, and so
    /// does an `await seq` that latched — the content moved on the instance
    /// that answers now. The probe read's answer ends nothing.
    pub(super) fn call(&mut self, args: &[&str]) -> Result<CtlReply, Fail> {
        // A stopped loop writes nothing into the worker: every press and
        // continuation goes through here and is refused before the transport
        // (the host's own transport refusal is a second fence). Reads and the
        // badge/claim cleanup still go out.
        //
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
        let mut full: Vec<&str> = Vec::with_capacity(args.len() + 1);
        if let Some(sid) = &self.setup.sid {
            full.push(sid.as_str());
        }
        full.extend_from_slice(args);
        let r = self.ctl.call(&full);
        // A request the Mac slept through: the watch sees it before anything
        // the answer leads to is decided or stamped.
        self.watch_sleep();
        let r = r.map_err(Fail::Hard)?;
        self.note_status(args, &r);
        // A write the server did not refuse ends the refusal the journal
        // last said: the next one is news.
        if claim::is_input(args) && !(r.is_err("halted") || r.is_err("busy") || r.is_err("rate")) {
            self.refusal_said = None;
        }
        // A press the server wrote behind input the worker has not read (warned rather than
        // refused): the next look reads `status` before it presses again, as after a refusal
        // (`Session::back_off`), so a stalled worker is held before a second key queues.
        if claim::is_input(args) && r.warned_input_unread() {
            self.stall_suspect = true;
        }
        // Built only where the outage's books read it: almost never.
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

    /// The request was not served ([`CtlReply::unserved`]): the reply never
    /// came or the server turned the connection away — or, while an outage
    /// is being ridden out, the server does not host the session (yet).
    pub(super) fn unserved(&self, r: &CtlReply) -> bool {
        r.unserved(self.outage.is_some())
    }

    /// A failed request's error, typed: [`Fail::Lost`] when it was not served
    /// ([`Self::unserved`]), else final.
    pub(super) fn fault(&self, r: &CtlReply, what: String) -> Fail {
        if self.unserved(r) {
            Fail::Lost(what)
        } else {
            Fail::Hard(what)
        }
    }

    /// Read the screen: `text --json tail=40` where the server accepts it (probed
    /// once), else the full `text --json`.
    pub fn read_screen(&mut self) -> Result<Screen, String> {
        self.screen().map_err(String::from)
    }

    /// One read of the worker as `aterm drive phase` prints it: the turn
    /// ([`Self::turn_of`]) and the reader its phase and every detail line go
    /// by — the session's program's ([`Self::program_reader`]), as the loops
    /// read every turn, so an agy, Muse Code or Gemini CLI worker is read by
    /// its own grammar.
    pub fn phase_now(&mut self) -> Result<(Turn, &'static dyn ScreenReader), String> {
        let screen = self.read_screen()?;
        let reader = self.program_reader(&screen.rows)?;
        Ok((self.turn_of(screen), reader))
    }

    /// The engine tests' bare wait ([`Self::await_turn`]) with the session's
    /// program named first ([`Self::program_reader`]), so the wait and the
    /// detail lines go by its reader. A `status` that does not answer leaves
    /// the reader to the screen ([`Self::reader`]): the wait itself rides a
    /// lost connection out.
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

    /// The reader for the session's foreground program as `status` names it
    /// now, else for what `rows` show ([`Self::reader`]): the grammar a
    /// caller outside the loops judges one screen of the session by
    /// ([`Self::phase_now`]).
    pub(crate) fn program_reader(
        &mut self,
        rows: &[String],
    ) -> Result<&'static dyn ScreenReader, String> {
        self.program = self.foreground_program().map_err(String::from)?;
        Ok(self.reader(rows))
    }

    /// The reader for the session's program as last read (`status program=`),
    /// else for what `rows` show ([`aterm_phase::identify`]): the one grammar
    /// every read of the loop goes through (its program's own, or none).
    /// Before the program is known, a box whose head is cut off the rows is read
    /// by Claude Code's grammar, whose box it is, so a live box in the first
    /// look is not taken for an idle screen.
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

    /// A row of `rows` matches the program's busy guard ([`ScreenReader::busy_guard`]). `rows`
    /// is the live zone ([`live_rows`]), never the whole screen: a busy row above it is another
    /// process's, and would hold a wait armed on it. The guard is compiled once while it stays
    /// the same; one that does not compile matches nothing. A row the guard's scan could not
    /// decide (its budget ran out) counts as up, as `await gone` reads it (`RowMatch::clears`),
    /// so the turn is never let go on a busy row nobody read.
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

    /// `screen` as a turn that ended, its phase read by [`Self::reader`] and
    /// overruled where the session's transcript last judged THIS screen
    /// (BG-1, its `seq`): a turn that ended waiting on background work is
    /// busy ([`Self::transcript_wait`]), a carried wait whose turn closed
    /// with nothing pending is idle ([`Self::transcript_wait_over`]) — as
    /// [`Self::await_turn_from`] read the point, so a later read of the same
    /// screen keys the same point ([`Self::review_key`]).
    pub(super) fn turn_of(&self, screen: Screen) -> Turn {
        let reader = self.reader(&screen.rows);
        let mut phase = reader.phase(&screen.rows);
        let carried = phase == Phase::Idle && self.transcript_wait_seq == Some(screen.seq);
        if carried {
            phase = Phase::Busy;
        } else if phase == Phase::Busy && self.transcript_over_seq == Some(screen.seq) {
            phase = Phase::Idle;
        }
        // A wall waited on, its rows gone from the screen; the transcript's
        // wait is none.
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

    /// A DECIDED POINT READ AGAIN (its due, the probe's close, the report's
    /// grace, a badge adopted): [`Self::turn_of`] of `screen`, the session's
    /// transcript asked of THIS screen first as [`Self::await_turn_from`]
    /// asks it. A screen that moved since the point's read misses the
    /// verdict kept for the old one, and a `turn_duration` row written since
    /// can count work pending, so the re-decision neither strands a carried
    /// wait the transcript ended (BG-1 correction 3: the act its due was for
    /// never taken) nor types into a turn Claude Code will wake.
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

    /// What makes `turn` the point it is ([`point_key`]), read by [`Self::reader`], never by the
    /// reader the screen alone names (which may differ from the session's program). Every
    /// comparison of two points is of these keys, except the wait's look under a box's first
    /// row, which compares what a file box asks ([`Self::file_request`]).
    pub(super) fn review_key(&self, turn: &Turn) -> String {
        point_key(self.reader(&turn.screen.rows), turn)
    }

    /// What the file box on `turn` asks ([`file_request_key`]), read by [`Self::reader`] as
    /// [`Self::review_key`] is; `None` for any other point.
    fn file_request(&self, turn: &Turn) -> Option<String> {
        file_request_key(self.reader(&turn.screen.rows), turn)
    }

    /// The busy row a turn's first wait waits to leave, before anything is read
    /// ([`Self::await_turn_from`]): the reader of the last screen read, over its live zone
    /// ([`live_rows`]). With no screen read yet, the program's guard as named (with none named,
    /// Claude Code's `esc to interrupt`, which every measured agent's busy row carries) over
    /// every row, unscoped, so its step is capped ([`BusyWait::step`]).
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

    /// One read of the worker's screen over the control socket. Not named
    /// `read`: the lock-order census identifies locks by method name
    /// (`read`/`write`/`lock`/`try_*`), and a `read` held across another
    /// `read` looks like a re-entrant lock.
    fn screen(&mut self) -> Result<Screen, Fail> {
        // A test's planted variant of the loop: one clock read more before
        // each screen (`fx::plant_clock_read`), nothing else changed.
        #[cfg(test)]
        if super::fx::planted_clock_read() {
            let _ = self.fx.mono();
        }
        let (screen, read_by) = self.read_once()?;
        // The screen a decision may judge, told to the tape with the read
        // it came in ([`super::fx::Fx::screen_read`]).
        self.fx.screen_read(&screen);
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
        let credits = reader.credits(&screen.rows);
        self.note_credits(reader, credits.as_ref(), &screen.rows);
        self.note_usage(reader, credits, &screen.rows);
        // Every read clocks the draft: one that changed is a person typing.
        let draft = reader
            .composer(&screen.rows)
            .filter(|c| typed_draft_in(Some(c), reader.placeholder(), &screen))
            .map(|(_, lines)| lines.join("\n"));
        self.draft_seen = match (draft, self.draft_seen.take()) {
            (Some(d), Some((seen, at))) if d == seen => Some((seen, at)),
            (Some(d), _) => Some((d, self.fx.mono())),
            (None, _) => None,
        };
        // The last read kept in the previous one's buffers.
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

    /// What `rows` say of the account's usage, told to the host ([`IdleHost::usage_seen`]) when it
    /// changed since the last telling, or when it says something and that telling is
    /// [`crate::harness::accounts::CREDITS_RETELL_S`] old. A loop with no host reads nothing.
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

    /// WHAT THE NOTICES SAY OF USAGE CREDITS, kept from every read
    /// ([`Self::credits_kept`]), read whatever the loop does at the point,
    /// since the notices stand some seconds, mostly under the running turn:
    ///
    /// * a notice opens what is kept, at this read; the one they started on,
    ///   with a reset, places it once for its text (the session window's as a
    ///   usage window's is, [`Self::reset_unix_for`]); a per-model one keeps
    ///   its model;
    /// * a read with the composer and no notice keeps it until its placed
    ///   reset, or the credits hold's bound where none could be placed
    ///   ([`TurnEndTiming::credits_bound`]); absence alone says nothing of
    ///   subscription restoration. A model's stands while its process does
    ///   (its notice is drawn once a process); a read with no composer,
    ///   which cannot draw a notice, keeps it;
    /// * a model's is the process's: a process the host says started since
    ///   it was first read keeps none (`IdleHost::agent_started`).
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

    /// One read, and the reader it was read by when a tail that stands chose
    /// one ([`Self::reader`] of its rows; `None` for a whole read), its
    /// limit's words read with the rows above where the screen cannot tell
    /// whose they are ([`Self::limit_words_read_above`]).
    fn read_once(&mut self) -> Result<(Screen, Option<&'static dyn ScreenReader>), Fail> {
        let read = self.read_shaped()?;
        let (screen, read_by) =
            self.limit_words_read_above(read.screen, read.read_by, read.above)?;
        Ok((self.message_bounded_by_cells(screen, read_by), read_by))
    }

    /// THE LAST MESSAGE BOUNDED BY ITS CELLS where its text cannot bound it
    /// (agy, whose message's blank line is a blank row like the one that
    /// ends it, its later rows two columns in as a reply's:
    /// [`ScreenReader::message_blanks`]). A person's paragraphs typed or
    /// pasted into agy read as agy's reply otherwise — their `NEEDS YOU:`
    /// a need it raised (lab r16o) — and aterm knows nothing of a message it
    /// did not type but what the screen draws. agy draws every row of a sent
    /// message from column 0 in its caret row's style, its blank rows among
    /// them, and the blank row that ends it plain (MEASURED,
    /// `aterm_phase::agy::fixtures::lab::PERSON_PARAGRAPHS`): the loop reads
    /// the caret row's column-0 cell, then each blank row the text names,
    /// top down, and the rows drawn in the caret row's style, up to the
    /// first that is not, are taken out of the message
    /// ([`ScreenReader::fold_message`]) before any reading — the reply, the
    /// composer and every row under the message keep their grid rows. Only
    /// a plain row found under them proves the styles tell the two apart:
    /// with none, a cell that cannot be read, or a glyph that is not the
    /// caret's or a blank (the screen moved under the reads), nothing is
    /// folded and the screen reads as its text does. A read of the same
    /// rows at the same `seq` folds the same rows with no request.
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

    /// The rows `screen` was read as before [`Self::message_bounded_by_cells`]
    /// folded its last message: the rows its fold was kept for, where that
    /// fold is this screen's (the same `seq` and `first`, the rows it folded
    /// to `screen`'s); else `screen`'s own. What a read of the history above
    /// holds against the grid read again ([`Self::rows_up_to_at`]), which
    /// draws the message unfolded.
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

    /// THE LAST MESSAGE OF ROWS READ UP INTO THE HISTORY BOUNDED BY ITS CELLS
    /// ([`Self::message_bounded_by_cells`] over the grid and the history above
    /// it): `rows`, the first of them absolute row `abs` (`None` on the
    /// alternate screen's archive, whose rows have none), their message's
    /// caret row and blank rows read where they are drawn (`cell abs=<n> 0`,
    /// a history row's as it was drawn), and the blank rows drawn in the
    /// caret row's style taken out of the message
    /// ([`ScreenReader::fold_message`]). A person's message that scrolled up
    /// out of the grid under a long reply is bounded so too: read as text,
    /// its later paragraphs were the reply's top, and their `NEEDS YOU:` the
    /// worker's need (the review of 3a4586a01e, T1). `rows` as they are
    /// where nothing bounds them, as [`Self::message_cells`] says.
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

    /// The blank rows of [`Self::message_bounded_by_cells`] drawn in the
    /// caret row's style, top down, up to the first that is not — empty
    /// where no row under them is plain, or a cell does not read as the
    /// caret's or a blank. Each row's column-0 cell is read where `at`
    /// says.
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

    /// THE ROWS ABOVE A LIMIT'S WORDS, read where the screen cannot tell
    /// whose words they are: on Claude Code's screen, the
    /// row [`aterm_phase::limit_words_above`] names — the newest limit
    /// notice, its `/goal` echo, or the vendor's word on its auto-continue,
    /// whose owner walk reached the first row read undecided — is read
    /// again with the whole grid over a tail, and with the rows right above
    /// the grid ([`Self::rows_up_to_owner`]: the history, or on the
    /// alternate screen aterm's archive) where the grid's first row is
    /// reached too. Where they make it a program's words
    /// ([`aterm_phase::phase::words_of`]), the screen is cut under its line:
    /// the rows cut are words whose owner is off the screen read, and no
    /// reader sees them, as no reader sees a tail's rows above it. Measured
    /// on 2.1.304: a Stop hook's 35-line `stopReason` ending `Usage limit
    /// reached · resets 3pm` read, from the loop's 40-row tail, as a session
    /// limit's notice with no turn served under it (`since_limit`), its
    /// summary row above the tail; one of 60 lines draws the summary in the
    /// history. Rows that cannot all be read — the grid no longer the
    /// screen's, an evicted row, no owner within
    /// [`super::wall_confirm::OWNER_ROWS`] rows, the screen moved under the
    /// reads — leave the screen as read, the row Claude Code's, as before. A
    /// read of the same rows at the same `seq` takes the last verdict again
    /// with no request. `above`: the grid's rows above `screen` where the
    /// read was whole ([`Self::read_shaped`]), so the grid is not read again.
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

    /// Whether the row `words` names on `screen` is A PROGRAM'S WORDS read
    /// with the rows above the screen ([`Self::limit_words_read_above`]):
    /// the last row of its line, which the screen is cut under, or `None`.
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

    /// [`Self::read_once`]'s read of the screen: a tail that stands for it,
    /// or the whole grid (cut to its live zone where that stands for it) and
    /// the grid's rows above what it keeps.
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
        // The tail cut a box's head off, holds none of the live rows, ends on
        // a blank row under drawn rows and decides nothing or an idle
        // composer, or holds a gap the live zone reaches over: the full read
        // below — cut to the live zone, the rows the server's verdict reads,
        // where the tail ended on a blank row or held a gap, unless that zone
        // misses the live rows too.
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

    /// A whole read cut to the screen's live zone ([`aterm_phase::live_zone_start`], the rows
    /// the server's agent verdict reads), or left whole where that cut would miss the live rows
    /// as a tail can ([`tail_misses_the_live_rows`]).
    /// The rows it cut off, too.
    fn live_zone(&self, mut whole: Screen) -> (Screen, Vec<String>) {
        let first = aterm_phase::live_zone_start(&whole.rows, TAIL_ROWS);
        // Cut in place; a cut that misses puts the rows above back.
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

    /// One whole read of the grid (`text --json`, no `tail=`) with none of [`Self::screen`]'s
    /// bookkeeping; the outage's books are still [`Self::call`]'s. The accounts probe reads the
    /// Settings panel so ([`Self::usage_probe`]): the panel is the harness's own doing, and its
    /// header sits on rows 2 and 3, above any tail. A wall's decision reads so for the call a
    /// tail can cut off ([`Self::under_call_on_screen`]).
    pub(super) fn grid(&mut self) -> Result<Screen, Fail> {
        let screen = self.text_json(&["text", "--json"])?;
        self.fx.screen_read(&screen);
        Ok(screen)
    }

    /// One `text --json …` read of a WHOLE frame ([`Self::whole_frame`]). A read the server
    /// says caught the program's DEC 2026 frame unfinished ([`Screen::frame_open`]) holds a
    /// prefix of a redraw, its busy row possibly not drawn yet (Claude Code 2.1.308 and agy
    /// 1.4.0 have a frame split across the PTY reader's bursts under load,
    /// `docs/measured/half-drawn-frame-2026-10-22.md`). A host without `await frame` sends
    /// no `"frame"`.
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

    /// One read of a WHOLE frame by `read`, which answers the read and whether the server
    /// said it caught the program's DEC 2026 frame unfinished: such a read is taken again
    /// after `await frame`, which answers at the frame's close or at the server's
    /// one-minute hang detector, at most [`FRAME_REREADS`] times, and the last read is
    /// returned as it stands after that (its own word that the frame was open with it, for
    /// the caller to treat as output still arriving). Every supervisor read of the screen
    /// goes through here: `text --json` ([`Self::text_json`]) and the report's
    /// `offscreen … screen=1`.
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

    /// One `await <cond> timeout <step>`. A request not served is told apart
    /// before the exit code is read: the client's own deadline also exits 124,
    /// and only the server's `OK timeout` is a step that ran out.
    fn wait(&mut self, cond: &[&str], step: Duration) -> Result<Wait, Fail> {
        // Rounded up to whole milliseconds, so a wait is never shorter than asked (a truncated
        // box settle would read the box as young).
        let ms = step.as_nanos().div_ceil(1_000_000).to_string();
        let mut args = vec!["await"];
        args.extend_from_slice(cond);
        args.push("timeout");
        args.push(&ms);
        let r = self.call(&args)?;
        if r.ok() {
            Ok(Wait::Latched)
        } else if self.unserved(&r) {
            Err(Fail::Lost(format!(
                "await {} failed: {}",
                cond.join(" "),
                r.stderr.trim()
            )))
        } else if r.timed_out() {
            Ok(Wait::TimedOut)
        } else if r.usage_error() {
            Ok(Wait::Unsupported)
        } else {
            Err(Fail::Hard(format!(
                "await {} failed: {}",
                cond.join(" "),
                r.stderr.trim()
            )))
        }
    }

    /// One `await gone` on the program's busy row ([`BusyWait`]). A span the
    /// grid no longer has — a resize since the read shrank it under the
    /// span's first row, which the server refuses as `ERR bad rows` — is the
    /// screen changing shape under the wait: it ends as a latch does, and
    /// the next read arms the wait again from what the screen is now.
    fn wait_gone(&mut self, busy: &BusyWait, step: Duration) -> Result<(), Fail> {
        // The wait's arguments after `await`: `gone <guard> [rows <a> <b>]`.
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

    /// One `await gone <cond…>`: whether it latched. Every host the
    /// supervisor runs against serves the verb, so a refusal is a fault.
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

    /// The turn the TIMEOUT carries when the budget runs out while an outage
    /// is ridden out: the last screen read, as it read (a busy phase and no
    /// rows when no read ever answered).
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

    /// THE ENGINE TESTS' BARE WAIT over the live loop's ([`Self::await_turn_from`],
    /// which the hosted loop runs between its looks): block until the
    /// worker's phase is no longer busy, or `timeout` passes.
    ///
    /// The loop: wait; read; done unless busy; else wait again. Where the
    /// server knows `await gone`, the wait after a busy read showing the busy
    /// footer is the footer leaving (`await gone esc.to.interrupt`): the turn
    /// ending and a box drawn where the footer was both latch it at once,
    /// however the screen above animates. That wait watches the live zone of
    /// the read that armed it (`rows <a> <b>`, [`live_rows`]), never a footer
    /// above it (a dead process's footer never leaves). The read right after
    /// that latch may be a repaint caught midway: one with no composer yet, or
    /// an idle one whose composer frame its reader judges not whole
    /// ([`ScreenReader::frame_torn`]), is read again after the
    /// short settle. Neither the latch nor a read takes a DEC 2026 frame the
    /// program has not closed ([`Self::text_json`]). A busy read without
    /// the footer in its live zone waits for the next content change and a
    /// short settle ([`SETTLE_MS`], at most [`SETTLE_CAP`]); a turn's first
    /// wait is the footer leaving (armed before any read, over every row,
    /// capped by [`UNSCOPED_CAP`]) or that settle right after a press or a
    /// review point. A screen without Claude Code's composer
    /// frame that never held still for 2 s and shows no box is output still
    /// arriving and counts as busy. An outage is ridden out ([`Self::ride_out`],
    /// lines on stderr) and the wait restarts from a fresh read; a timeout spent
    /// in one is the timed-out turn on the last screen read (`spent_turn`).
    #[cfg(test)]
    pub(crate) fn await_turn(&mut self, timeout: Duration) -> Result<Turn, String> {
        self.await_turn_to(timeout, &mut std::io::stderr())
    }

    /// [`Self::await_turn`], its `RECONNECT …` lines said to `log`.
    #[cfg(test)]
    fn await_turn_to(&mut self, timeout: Duration, log: &mut dyn Write) -> Result<Turn, String> {
        let deadline = self.fx.mono() + timeout;
        let mut gone_first = true;
        let mut moved = Saw::default();
        loop {
            let remaining = deadline.saturating_duration_since(self.fx.mono());
            // No loop around it, so no context line to say (the hosted
            // loop's context watch is armed by its `opts`).
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
                    // What the instance that answers now serves may still be
                    // arriving: settle first.
                    gone_first = false;
                }
                Err(Fail::Hard(e)) => return Err(e),
            }
        }
    }

    /// The live loop's wait for the turn's end, with the `await gone` first
    /// wait optional: right
    /// after an approval the busy footer is not up YET, so its leaving is no
    /// signal — the settle wait is the first one. Marks `saw` when a read on
    /// the way was busy (the worker moved since the last look: [`Saw::busy`])
    /// and when one showed the worker's own busy row ([`Saw::live`]); the
    /// caller owns it, so what was read before a lost connection is not lost
    /// with it. A read with no box on it ends the mount of the box before it:
    /// the box after it waits out its own settle (`box_young`). Every read
    /// before the deadline is shown to the context indicator's watch
    /// ([`Self::watch_context`]), a busy one included — a
    /// worker compacts in the middle of a turn — and what it has to say is
    /// said through `review` as the read comes; a read at or after the
    /// deadline is not shown to it, so it says nothing then. Unattended, a
    /// busy read is shown to the limit episode too ([`Self::limit_on_busy`]):
    /// a notice that said the worker would go on by itself is over at it.
    ///
    /// UNDER AUTOPILOT OFF (the word the loop last read, [`Self::autopilot`])
    /// the wait acts on nothing: its busy reads keep the loop's books — the
    /// turn's work, the ends of its own badges and steps — and press, type
    /// and step nothing: no host step at a background break
    /// ([`Self::host_steps_in_background`]), and no rating card dismissed
    /// ([`SurveyDuty`], which the look gives no Off loop). The turn is waited
    /// out as any is, and the look parks at its point
    /// ([`Self::park_while_off`]).
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
        // Since when every busy read showed the agent's own background work
        // and nothing else ([`Self::host_steps_in_background`]).
        let mut background_since: Option<Instant> = None;
        // The `Gone` in hand was armed on a busy row a read of this wait saw (not the first
        // wait's guess from before any read).
        let mut footer_seen = false;
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
            // A wait that said nothing about the screen holding still: the
            // footer leaving, or the short settle.
            let mut unsettled = false;
            // The screen held still for IDLE_MS right before the read.
            let mut settled = false;
            match &next {
                Next::Gone(busy) => {
                    self.wait_gone(busy, busy.step(step))?;
                    unsettled = true;
                }
                Next::Settle => {
                    self.wait(&["idle", SETTLE_MS], step.min(SETTLE_CAP))?;
                    unsettled = true;
                }
                // A screen without the composer frame is settled by the full
                // idle window, re-read every few seconds: output still
                // arriving never holds still, and the program writing it may
                // be the agent drawing its first frame, whose footer then
                // takes over the wait.
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
            // No box on the screen: the box before it has left, and the one
            // after it mounts afresh — an autocompact offer's stepper record
            // with it once the composer is back.
            if phase != Phase::Prompt {
                self.box_gone();
                self.offer_gone(aterm_phase::phase::has_composer_frame(&screen.rows));
            }
            // No composer and the screen never held still: output still arriving, not the end
            // of anything. Not while the harness's `/usage` panel is owed its close
            // ([`UsageProbe`]: it repaints where the composer was as it loads, and taken for work
            // it would hold off the look that closes it) or a limit reset's step is owed
            // ([`ResetStep`]: its pending lines are [`Self::reset_look`]'s).
            // And a read the server still said caught the program's DEC 2026 frame unfinished
            // after the whole-frame read stopped waiting for it ([`Self::whole_frame`]): a
            // redraw caught midway, output still arriving, never a point to judge.
            let writing = screen.frame_open
                || (!settled
                    && !matches!(phase, Phase::Busy | Phase::Prompt)
                    && reader.composer(&screen.rows).is_none()
                    && self.probe == UsageProbe::Idle
                    && self.reset == ResetStep::Idle);
            // A dialog no reader names whose screen never holds still (a tool
            // row blinking over an unrecognised box) is `writing` for ever and
            // never reaches the wait that would see it.
            if writing {
                self.dialog_note(&screen, review);
            }
            // An idle read the session's transcript says is a turn that
            // ended waiting on background work, where the screen draws no
            // row of how it ended ([`Self::transcript_wait`], BG-1): the
            // agent's own background work, as a remembered wall overrides a
            // read below — never a point, and never its live turn.
            let carried = phase == Phase::Idle && !writing && self.transcript_wait(reader, &screen);
            if carried {
                phase = Phase::Busy;
            }
            // ...and the other way: a busy read that is nothing but the wait
            // Claude Code carries, whose turn the session's transcript says
            // closed since with nothing pending ([`Self::transcript_wait_over`]):
            // the fresher word, an idle point.
            if phase == Phase::Busy && !carried && self.transcript_wait_over(reader, &screen) {
                phase = Phase::Idle;
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
                return Ok(Turn {
                    phase,
                    screen,
                    timed_out: false,
                });
            }
            if writing {
                phase = Phase::Busy;
            }
            // A busy wait can outlive the owner's Cruise -> Off change.
            // Refresh at reads that could act, not on ordinary spinner
            // frames: a host's background point or a rating card. A host's
            // background point is a drawn wait or one the session's
            // transcript carries (`carried`, BG-1), which
            // [`Self::host_steps_in_background`] offers the host alike (the
            // merge review's XM-OFF-CARRIED-BREAK). A refused mode read keeps
            // the books but admits no action.
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
            // A wall the loop waited on whose rows have left the screen (a
            // scroll back through the transcript, a local command's output
            // under it) while only the agent's own background work holds it
            // busy: the point is the wall still, and the standing wait goes
            // on. A live turn ends the memory; the transcript's wait is none.
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
            // THE AGENT'S LIVE TURN — its spinner or busy footer, never a
            // break of its own background work or `/login`'s own dialog short
            // of its success ([`super::policy::login::dialog_stands`]): the
            // loop's act awaited, if
            // any, was taken ([`TurnEndState::act_untaken`]), so a break it
            // leads to is the agent's own and may be offered to the host.
            if !writing && !carried && !signing_in && reader.background_wait(&screen.rows).is_none()
            {
                // (The occupancy hold's clock reads the edge into this turn
                // first, [`occupancy_loop`].)
                self.took(screen.seq, opts.map(|o| &o.policy), review);
                // A live turn no act of the policy's started is someone
                // else's: the task's face is open again from this read
                // (`✓ landed` would stand through a person's new task for
                // as long as it ran), told once — the same turn read again
                // is no move ([`Self::tell_face`]).
                self.tell_face();
                // A refused wall's episode is over: met again at a later
                // point, it is said again ([`Self::say_wall_refused`]).
                self.wall_refusal_over();
            }
            // A break of the agent's own background work is offered to the
            // host; whatever it types there, the wait below is for the screen
            // to move on from this read.
            if !writing {
                if !off {
                    self.host_steps_in_background(
                        &screen,
                        &mut background_since,
                        opts.map(|o| &o.policy),
                        review,
                    );
                }
                self.stuck_busy_note(&screen, background_since, review);
            }
            if !(writing && unsettled) {
                saw.busy = true;
                saw.live |= !writing && !signing_in;
                // Gemini CLI's usage-limit dialog kept trying: a try in
                // flight keeps its episode, a try that got through ends it
                // ([`Self::limit_retry_busy`]). A screen only `writing` is
                // no agent's read.
                let try_in_flight = !writing && self.limit_retry_busy();
                // The worker works: the wait the host was told ended (the
                // try was typed) — told here, not at the next point, which
                // the busy turn keeps this loop from reaching. `/login`'s own
                // browser spinner (`signing_in`) is no turn of the worker's.
                if !signing_in {
                    if let Some(opts) = opts {
                        self.end_told_wait(opts);
                    }
                    // The turn's work, as the turn-end policy measures it.
                    self.running.busy(self.fx.mono());
                }
                // Unattended: the worker works, so a self-resuming limit notice
                // is over, and so is the point this loop escalated (a person
                // answered and the worker took it). Its badge goes now, at the
                // first busy read, since the turn it started may run for hours.
                // A screen only `writing` is no agent's read: it waits for one.
                if review.unattended() {
                    if !signing_in {
                        self.limit_on_busy(screen.seq, review)?;
                    }
                    // A person's point the worker has gone back to work on is
                    // over — unless the loop is still waiting on a wall whose
                    // rows are out of view, which is no answer to it, or the
                    // spinner is its own `/compact`'s run: the point a
                    // compaction that fails leaves is the same one; or the
                    // busy read is `/login`'s own dialog short of its success
                    // ([`super::policy::login::dialog_stands`]): the sign-in
                    // the owner's notice stands for is still theirs.
                    if self.wall_memory.is_none() && !self.turn_end.compacting() && !signing_in {
                        self.exception_ended(screen.seq, Some("the agent working"), review);
                    }
                    // The escalated point's badge goes with it — save while a
                    // `Keep trying` of Gemini CLI's usage-limit dialog is in
                    // flight under its spinner: the badge its ladder raised
                    // stands till the try got through
                    // ([`Self::limit_retry_busy`]).
                    if self.box_ask.is_some() && !writing && !try_in_flight {
                        self.close_box(screen.seq, review)?;
                    }
                    // Likewise the told choice: a dialog the loop answered is
                    // done and is told now (`CHOSE`, a record: `status story=`),
                    // naming this read, since the turn may run for hours and a
                    // loop stopped in it would never tell it. The look's own
                    // edge is the fallback for a turn read busy only as
                    // `writing`.
                    if !writing && !self.question.answered.is_empty() {
                        self.question_done(screen.seq, review)?;
                    }
                    // A limit reset's step still owed: the worker works, so
                    // its dialog is gone, and the host is told now — the turn
                    // may run for hours, and the account's other sessions
                    // wait on that word ([`Self::reset_at_work`]).
                    if !writing && let Some(opts) = opts {
                        let live = !carried && reader.background_wait(&screen.rows).is_none();
                        self.reset_at_work(&screen, live, opts, review)?;
                    }
                }
                // The agent's memory past its ceiling while it works: a warning, said once an
                // episode and again when its verdict changes; any restart waits for the idle
                // point, and only the Mac's trouble makes one.
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
            // A rating card parked over a worker that reads busy (a spinner, a workflow it
            // waits on for an hour) is dismissed here, on the read that shows it, by the step
            // the idle look takes ([`Self::survey`]); never a box, a draft or a person's grace.
            // A press asks to read again before anything else.
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
                // Settle first: the read right after a footer this wait saw leave is often the
                // next frame mid-draw (a question dialog's first rows, no composer yet), whole
                // within the short settle. Any other, and a screen still writing after it,
                // waits out the full idle window.
                next = if footer_seen && matches!(next, Next::Gone(_)) {
                    Next::Settle
                } else {
                    Next::Idle
                };
                footer_seen = false;
                continue;
            }
            // Over the live zone only: a busy row above it (a dead process's
            // footer) never leaves.
            let (live, span) = live_rows(reader, &screen);
            if let Some(guard) = reader
                .busy_guard()
                .filter(|busy| !writing && self.busy_up(busy, &screen.rows[live..]))
            {
                // Level-triggered: it latches the moment the busy row leaves.
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

    /// `context_warn` at one read: the context indicator
    /// ([`super::phase::context_left`]) against the threshold. The first
    /// reading at or below it says `EVENT context seq=<n> <v>% until
    /// auto-compact` (the manager's moment to have the worker update its
    /// handoff notes), once a descent. After that, a framed read with no
    /// indicator, or one [`COMPACTED_RISE`] points over the last reading, is
    /// the compaction: `EVENT compacted seq=<n>` once (the manager's standing
    /// rules may be gone), and the watch re-arms. A read without the frame
    /// shows nothing; one with a box up (the box covers the indicator) is no
    /// sign it has gone. `0` disables it.
    ///
    /// A Codex screen without a figure is no sign it has gone: Codex's own
    /// item stays drawn through a compaction, and its default footer draws
    /// none.
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
                    // Gone, from where it would show: the frame up, no box.
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

    /// THE ENGINE TESTS' STOP-AT-REVIEW SEAM over the hosted loop's engine
    /// ([`Self::drive`]): the loop until its first review point. Returns the
    /// text and the exit code: `0` with the compact result when the worker
    /// needs the manager (a non-read prompt, a question, a limit notice, an
    /// idle composer); [`EXIT_TIMEOUT`] with `TIMEOUT` and the last read's
    /// compact result when the budget is spent — a turn read at or after the
    /// deadline is the TIMEOUT, never pressed, and so is a budget spent while
    /// an outage is ridden out. An outage is ridden out ([`Self::ride_out`]),
    /// its lines on stderr, never in the result; so are the survey's and the
    /// context indicator's `EVENT` lines.
    #[cfg(test)]
    pub(crate) fn supervise(&mut self, opts: &SuperviseOpts) -> Result<(String, u8), String> {
        self.supervise_to(opts, &mut std::io::stderr())
    }

    /// [`Self::supervise`], its `RECONNECT …` lines said to `log`. With
    /// `opts.journal`, the file gets every line said on `log`, an `APPROVED`
    /// line per approval and the `EVENT` line [`Lines`] would print for the
    /// review point, then `TIMEOUT`, or `EXIT <reason>` for the error the
    /// loop ends on; a journal that fails is said once on `log`.
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

    /// THE ENGINE TESTS' WATCHING SEAM: the hosted loop's lines
    /// ([`Self::run_hosted`]) with no host and no stop flag. A review point prints
    /// ONE line, [`event_line`], and the loop keeps watching: it waits for the
    /// screen to move past that point (`await seq`) before it looks again, so a
    /// manager's turn or key is picked up without a relaunch. A point that
    /// looks the same as the one last reported ([`Session::review_key`]: the
    /// same phase, summary and status row, the same last rows of the transcript
    /// up to it; for a box, the same request, however it is redrawn) is neither
    /// pressed nor printed again unless a read in between saw the worker busy
    /// (its own busy row: a box redrawn through a screen only being written is
    /// the box that was up), the loop approved a read, or an outage came in
    /// between — so a footer that ticks does not repeat an EVENT, nor a box
    /// re-wrapped, arrowed through or ticking its countdown, while a new box, a
    /// new reply, a manager's row or a retry's new notice does, however short
    /// the busy spell before it; and since nothing could be read in an outage,
    /// the point still showing after one is reported once more. An approval
    /// prints `APPROVED seq=<n> <command>`. An outage prints `RECONNECT
    /// <reason>`, is ridden out ([`Self::ride_out`]) and prints `RECONNECTED
    /// after <ms> ms` once a read answers again — each once an outage, however
    /// often it comes back before the loop gets past it. The last line is
    /// `TIMEOUT` (returns [`EXIT_TIMEOUT`]) when the budget is spent — no press
    /// and no EVENT comes after the deadline, and a budget spent in an outage
    /// is the TIMEOUT too — or `EXIT <reason>` (returns 1) when the session
    /// goes, the outage outlasts its reconnect window (`EXIT reconnect window
    /// lapsed: <the last failure>`) or the loop fails (a request). Every line
    /// is flushed as it is written. With [`SuperviseOpts::context_warn`] above 0, Claude Code's context indicator
    /// is watched on every read of a turn, a busy one included: `EVENT context
    /// seq=<n> <v>% until auto-compact` when it first reads at or below the
    /// threshold, once a descent, then `EVENT compacted seq=<n>` once it has
    /// gone (or jumped back up) — the worker compacted, and the watch is armed
    /// again.
    #[cfg(test)]
    pub(crate) fn watch(&mut self, opts: &SuperviseOpts, out: &mut (dyn Write + Send)) -> u8 {
        self.watch_to(opts, out, &mut std::io::stderr())
    }

    /// `Self::watch`, a journal's one warning said to `warn`. With
    /// `--journal`, every line — the last one too — is appended to the file
    /// before it is printed.
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
        // The badge this watcher raised goes with it: nobody is left to
        // clear it when its point leaves the screen.
        if !session_gone(&end) {
            self.release_attention(&mut review);
        }
        let (line, code) = watch_end_line(end);
        // Nothing is left to report a failed write to.
        let _ = sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .line(&line);
        code
    }

    /// THE BOOKS A LOOP BEFORE THIS ONE KEPT ON THE SESSION, taken up from
    /// its ledger at `path` at `now_ms` (unix ms) by a loop that starts on it
    /// (a new window, a seamless update, a policy reload, a fault restart),
    /// in this order: the acts within the typing budget's window, the
    /// exchanges left to a person, the usage act at a wall
    /// ([`seed_usage_act`]), the done check owed its answer
    /// ([`seed_done_check`]), the Land episode in hand
    /// ([`TurnEndState::seed_land`]), Off just lifted (`lifted_off`: the host
    /// started this loop as the session's Off lifted), the model switch
    /// ([`Self::seed_model_switch`]) and
    /// the account moves. The wall an
    /// earlier loop left open is handed back, for [`Self::seed_wall_memory`]
    /// once the rest of the loop's books are read. One method for the loop
    /// and its tests: a seed a test writes out again tests nothing the loop
    /// runs (the verifier of b2c17b2dd, A-1). The ledger read through the
    /// door, once, as the loop reads it ([`Self::seed_from_ledger_text`]).
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

    /// [`Self::seed_from_ledger`] over `body`, the text of the ledger at
    /// `path` the loop read ONCE through its door (`Fx::read_text`, main's
    /// 8533e1164: a harness tape holds the bytes once, and a replay answers
    /// from them); every reader takes that text (`approvals::*_in`), and the
    /// files beside it (the journal, the earlier tabs' ledgers) are read
    /// through the door too.
    pub(super) fn seed_from_ledger_text(
        &mut self,
        path: &Path,
        body: &str,
        now_ms: i64,
        lifted_off: bool,
    ) -> Option<approvals::OpenWall> {
        // A TAB A COLD RESTORE REOPENED (follow-7): the
        // restore minted it another session id, so its loop's ledger is new,
        // and the books of the loop that ran the conversation before aterm
        // ended are those of the tabs the restore noted it was reopened from
        // (`upgrade_drive::note_earlier_tab`). Taken up once, the take-up's
        // row first in this ledger ([`approvals::RULE_EARLIER_BOOKS`]); while
        // it stands, every seed below reads the earlier tabs' rows and then
        // this ledger's, and their journals so, as one continuous ledger
        // ([`approvals::books_through`]). Read from this ledger alone, the
        // loop backed off its first point from its attach, started a limit's
        // ladder over, and typed Land's instruction anew where the loop
        // before had nudged or spent the landing.
        let fx = self.fx.clone();
        let read = |p: &Path| fx.read_text(p);
        let sid = self.setup.sid.clone();
        let earlier =
            crate::harness::upgrade_drive::earlier_ledgers_in(path, sid.as_deref(), &read);
        let ledgers: Vec<(String, String)> = earlier
            .iter()
            .filter_map(|(t, p)| Some((t.clone(), read(p)?)))
            .collect();
        // THE SESSION'S FIRST LOOP ([`Self::first_books`]): no loop before
        // this one kept a row of it here, no tab it was reopened from is
        // noted, and Off did not stand since its last worker.
        self.first_books =
            !lifted_off && earlier.is_empty() && body.lines().all(|l| l.trim().is_empty());
        // …and the newest row of those books, past which an agent process
        // launched is none a loop before this one watched
        // ([`Self::books_last_ms`]).
        self.books_last_ms = std::iter::once(body)
            .chain(ledgers.iter().map(|(_, b)| b.as_str()))
            .filter_map(approvals::last_row_ms)
            .max();
        let own_journal = self.fx.read_text(&approvals::journal_beside(path));
        // A person's own exchange an earlier loop left to them, on this tab
        // or one a cold restore reopened it from: still theirs while it is
        // the conversation's last turn (RR3-TE-2).
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
        // The machine's sleep since the last act, which this loop never
        // measured: any turn's done row is read less what lies inside it.
        self.seed_sleep(body, now_ms);
        // (A made account move's row is `typed` and no typed act:
        // [`approvals::typed_at`] leaves it out.)
        let typed = approvals::typed_at_in(body, self.setup.sid.as_deref());
        let ages: Vec<Duration> = typed
            .iter()
            .filter_map(|&ts| u64::try_from(now_ms - ts).ok())
            .map(Duration::from_millis)
            .collect();
        self.turn_end.seed_acts(&ages, self.fx.mono());
        // THE `/login` A LOOP BEFORE THIS ONE TYPED, its answer still to come
        // (Off and back, a seamless handoff, a restart over its browser
        // hand-off): this loop's own where the screen shows it at a lost
        // login ([`TurnEndState::seed_login`]).
        if let Some(ts) = approvals::login_typed_last_in(body, self.setup.sid.as_deref()) {
            let ago = Duration::from_millis(u64::try_from(now_ms - ts).unwrap_or(0));
            self.turn_end.seed_login(ago, self.fx.mono());
        }
        // …and no task they carried where the loop that typed them stopped
        // carrying it since: the task's end, a person's exchange left to
        // them, Off after the act's answer ([`approvals::carry_ended_in`]).
        if approvals::carry_ended_in(body, self.setup.sid.as_deref()) {
            self.turn_end.seed_carry_over();
        }
        // The short streak the ledger last says, however old: the last act's
        // mark, or the record of the point a loop judged after it — that
        // point's streak and its time ([`TurnEndState::seed_short`],
        // [`TurnEndState::seed_point`]).
        let streak = approvals::short_streak_in(body, self.setup.sid.as_deref());
        // A LANDING A LOOP BEFORE THIS ONE LANDED, and nothing since: held
        // for this loop's first point, so a restart or a seamless update
        // types no land instruction into finished work
        // ([`TurnEndState::seed_landed`]). Nothing since is no act typed
        // after the landing's note, and no later point judged nor turn read
        // running: the record of the last point a loop judged (the
        // landing's own is written before its note) and, a turn read
        // running after it, a turn no note can come before
        // ([`ended_before`]). Read from the journal alone, a successor
        // seeded the landing over the records that a later point and a
        // later turn made since, and ended a new task's first short done
        // report as landed where the first loop typed the nudge (the merge
        // review's MG9-S1). Nor any other turn's point or box journaled
        // since, and the first point's last words still the landing's own
        // (the mark the journal kept with its point): someone else's later
        // done reply, a turn no loop watched among them, is never read as
        // the landing (the review of Together D3).
        let since = streak
            .as_ref()
            .filter(|s| s.recorded && s.point)
            .map(|s| if s.ran { i64::MAX } else { s.at_ms });
        //
        // The landing's note's own time goes with the start face
        // ([`TurnEndState::seed_noted_at`]): a window that held no face (a
        // seamless update's new one) shows it too. An older build's done of
        // any other kind seeds nothing (Autopilot Cruise / Land).
        //
        // A note that restates the landing after the task's own wake kept
        // it ([`TurnEndState::seed_landed_by_wake`]) seeds the wake's words.
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
            // A turn read running since the point recorded: the next idle
            // point is its end ([`approvals::RULE_TURN_RAN`]).
            if seed.point && !seed.ran {
                let ago = Duration::from_millis(u64::try_from(now_ms - seed.at_ms).unwrap_or(0));
                self.turn_end.seed_point(ago, self.fx.mono());
                // A person's Esc stood at it: held from the point, as the
                // loop that judged it holds ([`TurnEndState::seed_esc`]).
                if seed.esc {
                    self.turn_end.seed_esc();
                }
            } else if !seed.point && seed.recorded {
                self.turn_end.seed_yield(&seed.rule);
            }
        }
        // A LEDGER AN OLDER BUILD WROTE (no point record in it): the streak
        // and the point it was held at, or the API error's rung, and the
        // limit's tries it saw taken, as that build's loop journaled them
        // ([`journaled_hold`], [`journaled_takes`]).
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
                // A back-off's point is its wait's start; another wait's
                // its line.
                let point_ms = short_held(&hold.why)
                    .map_or(hold.at_ms, |(_, wait_ms)| hold.until_ms + 500 - wait_ms);
                let ago = Duration::from_millis(u64::try_from(now_ms - point_ms).unwrap_or(0));
                self.turn_end.seed_point(ago, self.fx.mono());
            }
        }
        // THE RETRY LADDER OF AN API ERROR'S WALL the loop before waited at,
        // as its last row marks it ([`approvals::RetryMark`]), or an older
        // build's journal says it ([`retry_held`]): worked from its rung and
        // its appearance ([`TurnEndState::seed_retry`]). That row the retry
        // itself, the retry is owed its answer: awaited while its take window
        // is open, as by the loop that typed it, else the wall's first
        // reading here.
        // The journal's wait is a point (the wall met again) unless the act
        // it ended in is the ledger's last.
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
        // …and one a loop of an older build left, as its journal says it:
        // its rows name no turn ([`journaled_left`]).
        if let Some(key) = older.and_then(|j| journaled_left(j, self.setup.sid.as_deref()))
            && !left_keys.contains(&key)
        {
            left_keys.push(key);
        }
        self.left_keys = left_keys;
        // The usage act a previous loop made at the wall this one
        // attaches to (a new window, a seamless update, a fault).
        seed_usage_act_in(
            &mut self.turn_end,
            body,
            self.setup.sid.as_deref(),
            &older_reads,
            now_ms,
            self.fx.mono(),
        );
        // The done check a previous loop asked, owed its answer, and the
        // Land episode it had in hand, read where every park its journal
        // says stood Off ([`with_parks_journaled`]): a build through 0.105.0
        // notes its park and records none.
        let parked = with_parks_journaled(body, journal.as_deref(), self.setup.sid.as_deref());
        seed_done_check(
            &mut self.turn_end,
            &parked,
            self.setup.sid.as_deref(),
            &older_reads,
            now_ms,
            self.fx.mono(),
        );
        // The Land episode a previous loop had in hand.
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
                // The first its answer, a second a later turn's; judged and
                // the task not ended there, no landing is in hand.
                if let Some(&first) = after.first() {
                    land.answered |= after.contains(&true);
                    land.landing = first && after.len() == 1;
                }
            }
            self.turn_end
                .seed_land(land.nudges, land.landing, land.cut, land.answered);
            // …and where its hold began: the hold's `since` and its nudges.
            self.turn_end.seed_land_hold(land.hold);
            // …and that act, owed its answer: taken only where the screen
            // or the record says so ([`TurnEndState::seed_land_act`]).
            if let Some(act) = land.act.filter(|_| land.landing && !land.cut) {
                let ago = Duration::from_millis(u64::try_from(now_ms - act.at_ms).unwrap_or(0));
                self.turn_end
                    .seed_land_act(act.said, act.lead, ago, self.fx.mono());
            }
            // …and which act a retry or a compaction's continuation delivers again: a `/compact`
            // typed at its point owes it to the continuation, and its point recorded last, read
            // again, a wall there cut it.
            if let Some(rule) = land.owed {
                self.turn_end.seed_land_owed(rule);
            }
            if land.compact {
                self.turn_end.seed_compact_land();
            } else if land.judged {
                self.turn_end.seed_land_point();
            }
        }
        // A turn read running since the point recorded last: a later turn
        // than the one the check's or the land act's answer was judged at.
        if let Some(seed) = streak.as_ref().filter(|s| s.point && s.ran) {
            self.turn_end.seed_ran();
            // …and when, for this loop's first point: that turn's end
            // ([`TurnEndState::seed_ran_turn`]).
            if let Some(ran_ms) = seed.ran_ms {
                let ago = |ms: i64| Duration::from_millis(u64::try_from(now_ms - ms).unwrap_or(0));
                self.turn_end
                    .seed_ran_turn(ago(ran_ms), ago(seed.at_ms), self.fx.mono());
            }
        }
        // A LOOP THE HOST STARTED AS THE SESSION'S OFF LIFTED: the person
        // drove the session since those acts, so they carry no task — but
        // for a turn already running as the loop attaches
        // ([`TurnEndState::start_lifted_off`]). Parked after the seeds, as
        // the park parks the books of the loop that parks (an act awaited
        // since before it, a check a wall cut), and the ledger says so for
        // the loops after it ([`approvals::RULE_OFF_STOOD`]: the host
        // stopped the loop before this one under Off, which wrote no park).
        // Parked before the seeds, the check a wall cut was seeded back
        // after the park dropped it, and the retry's done reply ended the
        // task where the loop that parked asked the check again (the
        // verifier of 341e1fa81, RS7-3).
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
        // The wall an earlier loop waited on, when the screen shows none
        // of its rows (the scroll that hid it, then the update that
        // applied): waited on still (handed back).
        let open_wall = approvals::open_wall_in(body, self.setup.sid.as_deref());
        // A model switch an earlier loop made is switched back at its
        // reset by this one.
        self.seed_model_switch_in(body, older);
        // The account moves an earlier loop made count toward this
        // one's bound (240 relaunches in five days is what a bound that
        // starts over with each loop allows); a move it could not make
        // is not asked again at the wall it left standing (a refusal of the
        // launch it was asked for, of that launch alone); and one the
        // ledger does not show SERVED is this loop's to answer — with a
        // turn already asked under it, or a bad answer already told,
        // where the ledger says so; made at a wall with neither, it
        // still owes that wall's resume at the notice it left
        // ([`TurnEndState::seed_account`]).
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

    /// The loop `supervise` and `watch` share: await the turn; under the
    /// approval policy, answer a box ([`Self::auto_read`]); anything else is a
    /// review point for `review`, which stops the loop or reports it and keeps
    /// watching, after which the loop waits for the screen to move past it. An
    /// unserved request is ridden out ([`Self::ride_out`]) and the loop looks
    /// again from a fresh read; budget spent there is the loop's TIMEOUT.
    fn drive(&mut self, opts: &SuperviseOpts, review: &mut dyn Review) -> Result<End, String> {
        // THE HOST CROSSES THE LOOP'S DOOR: every call this loop makes to its
        // window's host goes through the host this door hands back
        // (`Fx::host_door`) — the host itself in the shipped loop, the taped
        // host when the run records, the replay's when it replays — so the
        // loop's every read of a host answer, at each of the ~fifty places it
        // asks one (`opts.idle_host`, and `stall_host`, which is set from it
        // below), is on a tape with no call site knowing.
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
        // The loop's end ends what it told of a person's point: a host that
        // filed it retires it, and the journal says how long it stood.
        self.stuck_over(0, "the loop ended", review);
        self.exception_ended(0, Some("the loop ended"), review);
        // The occupancy hold this loop leaves open, booked again for a
        // successor to carry ([`occupancy_loop`]) — of a session still there.
        if !session_gone(&end) {
            self.hold_left(Some(&opts.policy), review);
        }
        // An act written outside a look (a step the loop's own start or end
        // took) is sent here, last.
        self.send_act_marks();
        end
    }

    /// Send the session log's queued `act` marks ([`ActMarks`]), oldest
    /// first, each as `mark - act word=<w> [rule=<id>]` through
    /// [`Self::call`] (so a tape records it as it records every request).
    /// One a server refuses latches the rest off ([`act_mark_latches`]);
    /// `ERR busy` (and any other refusal) drops that mark, never retried; a
    /// transport failure drops what is queued.
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

    /// [`Self::drive`]'s body.
    fn drive_loop(&mut self, opts: &SuperviseOpts, review: &mut dyn Review) -> Result<End, String> {
        // Mutable: a limit notice naming a reset past it stretches it
        // (`extend`), once a reset.
        let start = self.fx.mono();
        let mut deadline = deadline_after(start, opts.max);
        let allow = opts.allow();
        self.stall_host.clone_from(&opts.idle_host);
        self.stall_grace_s = opts.policy.human_grace_s;
        self.stall_term_after_s = opts.policy.stall_term_after_s;
        self.stall_term = opts.policy.stall_term;
        self.resume_limits = opts.policy.resume_limits;
        // Every read of this loop's turns is shown to the context watch
        // (`await_turn_from`), armed from the start.
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
        // The typing budget is the session's, not this loop's: the acts a
        // previous loop on it typed within the window count, so a loop
        // re-spawned by its host does not start with a fresh budget.
        if !std::mem::replace(&mut self.turn_end_seeded, true)
            && let Some(path) = self.setup.ledger_path.clone()
        {
            let path = path.as_path();
            // ONE READ OF THE LEDGER answers every question below, through
            // the door (`Fx::read_text`: a harness tape holds the bytes once,
            // and a replay answers from them): a file that is missing or not
            // UTF-8 reads as an empty ledger, which answers each of them as
            // a missing file did when each reader opened it for itself.
            let body = self.fx.read_text(path).unwrap_or_default();
            let now_ms = self.fx.stamp_ms();
            let lifted_off = opts.idle_host.as_ref().is_some_and(|h| h.lifted_off());
            open_wall = self.seed_from_ledger_text(path, &body, now_ms, lifted_off);
            // The occupancy hold this tab's own ledger left open, carried or
            // closed at the first thing its clock is fed ([`occupancy_loop`]).
            self.hold_seed_from(&body, start, now_ms, lifted_off, &opts.policy, review);
            // A `/usage` panel an earlier loop wrote for and no loop has
            // seen gone ([`approvals::probe_owed`]: the loop died, or was
            // stopped, between its write and the panel's close) is this
            // loop's to close — after one person's grace in its own sight,
            // and with nothing sampled from it. With no such row — every
            // ledger where no probe was ever typed — nothing is owed, and
            // this loop closes no panel it finds.
            if approvals::probe_owed(&body, self.setup.sid.as_deref()) {
                self.probe = UsageProbe::Owed {
                    own: false,
                    since: self.fx.mono(),
                };
            }
            // A limit reset's row an earlier loop took and no loop has told the end of
            // ([`approvals::reset_owed`]) is this loop's to end, never to enter: a confirmation
            // still up is Escaped after one person's grace in this loop's sight
            // ([`Self::reset_look`]); with no such row a confirmation this loop finds is a
            // person's. With `[harness] limit_reset` off, or the row stale
            // ([`reset_loop::RESET_STALE`]), the step ends untold ([`Self::reset_take_up`]). Only
            // an unattended loop takes the step up.
            if review.unattended()
                && let Some(open) = approvals::reset_owed(&body, self.setup.sid.as_deref())
            {
                self.reset_take_up(&open, review);
            }
        }
        // THE TASK'S FACE AT THE LOOP'S START, told whatever it is: open, the
        // policy's memory fresh, or LANDED where a landing a loop before this
        // one landed is seeded above — the landing restated, never announced
        // again (the window records a landing only on a move from a face it
        // knew). The host told
        // the window "unknown" when the loop before this one ended; the first
        // point takes the seed up (no move) or drops it (open) — design
        // §9.4.2. A seed's face goes with the time its end was noted
        // ([`TurnEndState::face_start_noted`]). A SEEDED LANDING's face waits
        // for the session's word, which the start-up reconcile below reads:
        // under Cruise the seed is dropped and the start face is open
        // ([`TurnEndState::start_under`]) — told landed first, a Cruise task
        // read `status task=landed`, with the glass's end mark, until its
        // first point (the merge review's P2).
        let seeded_landing = self.turn_end.landed_seed_armed();
        if !seeded_landing {
            self.tell_start_face();
        }
        // A real host requires the seeded wall's conversation here, whatever
        // the program, and persists it: a loop that ends before its first
        // decision leaves a hostless successor no legacy row to act on. The
        // first decision's read of an agent whose screen is all the evidence
        // (Muse Code, agy, Gemini CLI) releases it to the hostless lane's
        // ([`Self::scope_limit_conversation`]). A wall the row names met
        // under such an agent is not required at all
        // ([`Self::require_seeded_limit_conversation`]).
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
        // An attention a previous watcher left is settled before the first
        // wait: cleared when the screen no longer shows its point, adopted
        // when it does. A request not served here is the first look's to
        // ride out.
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
        // …ONLY ONCE THE HOST ANSWERED THE WORD: a start-up reconcile that
        // was lost (a seamless update's successor starts its loops before its
        // control socket listens) read no word, and the table's is no
        // session's — decided under it, a session under its own Land dropped
        // its seed under the table's Cruise and had the done check typed into
        // its landed work (the final check of 2026-10-20, W1). Unanswered,
        // the seed stays armed and its face is told to no one: the first
        // look reads the word before anything of it is decided — the Off
        // park among them — and decides the seed and tells its face there
        // ([`Self::look`]), as this start would have. Told as seeded first, a
        // Cruise task read `task=landed` until then; decided at the first
        // point instead, an Off session got past its park under the table's
        // Cruise and the point's fold under its Off dropped the seed (the
        // second review of 2026-10-20, R1). A loop with no look's read to
        // wait for (an attended one) tells its face now.
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
            // A renewal that found another holder (this loop's lease lapsed
            // in an outage and someone claimed the session) ends a hosted
            // loop here, before it looks again.
            if let Some(held) = self.yield_to_holder(opts) {
                return Err(held);
            }
            let looked = self.look(opts, &allow, &mut state, &mut deadline, review);
            // The act marks the look queued go once it returns: one fixed
            // point a tape replays in place, off every decision.
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
                        // Nothing could be read during the outage, so the
                        // point handed before is handed again if still
                        // showing: once an outage, not on every relapse.
                        Rode::Back { fresh: true } => state.handed = None,
                        Rode::Back { fresh: false } => {}
                    }
                    // The screen may have changed and its seq may have started
                    // over: settle, then read, as after a press. No seq from
                    // before is waited on, and a press that was in flight is
                    // not repeated — the box is read and classified again.
                    state.gone_first = false;
                }
                Err(Fail::Hard(e)) => return Err(e),
            }
        }
    }

    /// One look of [`Self::drive`]: await the turn, then approve a read, hand a
    /// review point over, or pass over the point already handed; `Some` ends
    /// the loop. The session survey ([`Self::survey`]) is said, or dismissed
    /// under `dismiss_surveys`. The
    /// context watch's lines ([`Self::watch_context`]) come as the turn's reads
    /// do. Unattended (`watch`, the hosted loop), a new point is decided by the
    /// turn-end policy ([`Self::turn_end_at_point`]) before it is reported and
    /// executed after ([`Self::turn_end_execute`]); a limit episode is handled
    /// around the point ([`Self::limit_on_busy`], [`Self::limit_after`]). The
    /// wait for the screen to move past the point is bounded by the policy's
    /// `WaitUntil` ([`Self::wait_for_next`]).
    ///
    /// A LIMIT RESET has one place here, unattended only: a step the harness
    /// opened — the usage-limit dialog's grant row taken — and has not told
    /// the end of is dealt with right after the panel's own
    /// ([`Self::reset_look`]), so its pending line and its confirmation are
    /// never a rule's to decide. The row itself is the approval policy's
    /// ([`Self::auto_read`], under `[harness] limit_reset`).
    ///
    /// THE ACCOUNTS PROBE has two places here, both unattended only. A
    /// `/usage` panel the harness opened and has not seen gone is dealt with
    /// FIRST, before anything of the look is decided
    /// ([`Self::usage_panel_look`]); and at a new point, between the host's
    /// step and the point's act, the host may claim a probe
    /// ([`Self::usage_probe`]) — after which the point is decided again
    /// ([`Self::after_probe`]) rather than acted on as first decided. With
    /// no panel owed and no claim, neither sends a request.
    ///
    /// THE POINT'S OWN LINE and the task's end its fold took go to the
    /// journal together ([`Self::say_point`]), before the host's step, the
    /// probe or the act can take the point — and before the loop ends on a
    /// request between the fold and the line that failed
    /// ([`Self::said_before_fail`]). Under an Off read for the point's
    /// decision nothing of the point is said.
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
        // A press's read handed on is this look's turn as read.
        let turn = match state.read_back.take() {
            Some(screen) => self.turn_of(screen),
            None => {
                // The rating card is also dealt with on the busy reads of the wait: a worker
                // that reads busy for an hour ([`SurveyDuty`]). Never under Off, where no card is
                // touched and the look parks at the turn's point.
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
        // What the claim's renewals changed while the turn was awaited.
        self.claim_said(review);
        if self.stopped() {
            return Ok(Some(End::Quit));
        }
        // A `/usage` panel of the harness's owed its close goes first, before anything of this
        // look is decided, pressed or booked (and before what the reads saw is taken, so a live
        // turn read on the way still counts at the next point): no decision is made on a panel
        // screen ([`Self::usage_panel_look`]).
        // Ahead of Autopilot Off's park too: an Off session is left as plain Claude Code, never
        // with the harness's `/usage` standing on it, but under Off the panel gets one plain
        // close and no more (no stall hold, no retry, no wait but a ledger panel's grace, which
        // presses nothing; the park instead, the panel owed until the word turns).
        let off = self.autopilot(opts) == Autopilot::Off;
        if review.unattended()
            && !turn.timed_out
            && self.fx.mono() < *deadline
            && self.usage_panel_look(&turn, state, *deadline, off, opts, review)?
        {
            state.gone_first = false;
            return Ok(None);
        }
        // Autopilot Off, by the word last read (the start-up reconcile's, a decision's): hands
        // off before anything else of this look is decided, pressed, booked or reported, a limit
        // reset's step included (it may key the purchase's confirmation), parked until the word
        // changes ([`Self::park_while_off`]), then read afresh.
        if review.unattended() && !turn.timed_out && self.fx.mono() < *deadline && off {
            self.park_while_off(turn.screen.seq, *deadline, opts, review)?;
            state.gone_first = false;
            return Ok(None);
        }
        // A limit reset's step that is owed goes next: the pending line and the confirmation
        // the harness's own row opened are never a rule's to decide ([`Self::reset_look`]).
        if review.unattended()
            && !turn.timed_out
            && self.fx.mono() < *deadline
            && self.reset_look(&turn, state, *deadline, opts, review)?
        {
            state.gone_first = false;
            return Ok(None);
        }
        // An act held for a person is looked at again once their grace is
        // over; what this look holds is its own.
        if std::mem::take(&mut self.rejudge) {
            state.handed = None;
        }
        self.held = false;
        // The point's review key ([`Self::review_key`]), built once, and only
        // where a check reads it.
        let mut turn_key: Option<String> = None;
        // A read since the last point saw the worker busy: the point showing now is a new one,
        // save the box handed over, still showing, when no read since showed the worker's own
        // busy row ([`Saw::live`]): that box was redrawn through output being written, not
        // asked again after work.
        let saw = std::mem::take(&mut state.moved);
        let worked = saw.busy;
        // A box read up: a person's Esc read after it — their refusal of it —
        // is a new one, its grace counted from that read, though no busy read
        // came before the box ([`TurnEndState::box_up`]).
        if turn.phase == Phase::Prompt {
            self.box_up(turn.screen.seq);
        }
        // The accounts probe goes only at the point of a turn read live ([`Self::turn_live`]):
        // this look's, never a later look at the same point.
        self.turn_live = saw.live;
        // …and a turn since the last point makes this one new to the probe's one line a point,
        // though its words be the last one's ([`Self::probe_shut`]).
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
            // A question dialog the loop answered has been submitted: its
            // memory is done, and when only the loop's keys answered it, it is
            // told once (unless already told at the turn's first busy read,
            // [`Self::await_turn_from`]).
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
        // The point escalated to the manager has left the screen — a box
        // answered, cancelled or another in its place, a question answered:
        // the badge it raised goes. The same box redrawn has not left: its
        // review key holds across a re-wrap, a moved focus, a ticking
        // countdown and a transcript reflowed above it ([`box_key`]).
        if self
            .box_ask
            .as_ref()
            .is_some_and(|b| b.key != *turn_key.get_or_insert_with(|| self.review_key(&turn)))
        {
            self.close_box(turn.screen.seq, review)?;
        }
        // The budget bounds every action: a turn read at or after the
        // deadline is neither pressed nor reported — it is the TIMEOUT.
        if turn.timed_out || self.fx.mono() >= *deadline {
            return Ok(Some(End::Timeout(turn)));
        }
        // A limit episode ends the moment the worker works again (a turn read
        // busy, or a box up) before anything is pressed
        // ([`Self::close_episode`]). A busy spell that ends on the notice again
        // is a retry that hit the wall: the same episode. A self-resuming
        // notice closed at the busy read already ([`Self::limit_on_busy`]).
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
        // The worker answered on a point that is not the notice: an episode a busy read closed is
        // over for good ([`Retry`]), and so is a limit badge adopted from a previous loop
        // ([`Self::adopted_limit_over`]).
        if review.unattended() && !matches!(turn.phase, Phase::Limited { .. }) {
            self.retry = None;
            self.adopted_limit_over(turn.screen.seq, ANSWERED_PAST_LIMIT, review)?;
        }
        // A worker that stopped reading its input is held (`stall.rs`):
        // nothing pressed, typed or escalated; the wait probes on.
        if review.unattended() && self.stall_look(turn.screen.seq, state, review)? {
            state.gone_first = false;
            return self.wait_for_next(turn, opts, allow, deadline, review);
        }
        if self.survey(&turn, opts, &mut state.survey, false, review)? {
            state.gone_first = false;
            return Ok(None);
        }
        // A question dialog handed back while it waited (its policy no longer
        // `ask`) is decided afresh, not passed over as the point already
        // handed.
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
            // …save its composer: a draft that held it, sent or cleared,
            // decides it again ([`Self::draft_look`]).
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
            // A point decided at or after the deadline — the press's wait for
            // the box to leave ran out with the budget — is the TIMEOUT too.
            if self.fx.mono() >= *deadline {
                return Ok(Some(End::Timeout(point)));
            }
            // The turn-end policy's decision, made once when the point is new
            // ([`Self::turn_end_at_point`]): the turn's work is the time since
            // the first busy read after the last point (or the nudge's box,
            // [`RunningTurn::point`]); a latched person's hand is spent.
            let busy_for = self.running.point(self.fx.mono());
            let turn_end = if review.unattended() {
                let action = self.turn_end_at_point(&point, busy_for, opts)?;
                // The task's edge is this point's, published as it is folded
                // in: the host's step and the probe below may take the point
                // and type into it, moving the transcript a done's result is
                // read from ([`Self::task_edge_at_fold`]). Its note waits for
                // the point's own line, and goes right after it — before the
                // loop ends on a request of the fold's that failed, too
                // ([`Self::said_before_fail`]).
                let folded = self.task_edge_at_fold(&point.screen, opts, review);
                self.said_before_fail(&point, folded, review)?;
                action
            } else {
                TurnEndAction::Nothing
            };
            // A LIMIT THE POLICY READ OVER AT THIS POINT ends its episode as a
            // busy read does ([`Self::close_episode`]): its retry taken with no
            // wall since, a turn served under its notice or after its row in
            // the transcript, the vendor's own continuation of it
            // ([`TurnEndState::limit_ended`], folded in just now). A reply too
            // quick for any read (the lab's stub answers in 0 s) left the
            // episode open, the host's steps withheld to the reset plus an
            // hour — for good where no reset is named — and the `limited:`
            // badge up as long.
            if review.unattended()
                && self.limit.is_some()
                && self.turn_end.limit_ended()
                && !matches!(point.phase, Phase::Limited { .. })
            {
                let closed =
                    self.close_episode(point.screen.seq, "answered since the limit", review);
                self.said_before_fail(&point, closed, review)?;
            }
            // Autopilot Off, read for this point's decision (which then decided nothing): the
            // point is neither reported, escalated nor acted on (a limit's episode, the host's
            // step and the probe included), and the loop parks until the word changes, then
            // decides the screen afresh ([`Self::park_while_off`]).
            if review.unattended() && self.autopilot(opts) == Autopilot::Off {
                self.park_while_off(point.screen.seq, *deadline, opts, review)?;
                state.gone_first = false;
                return Ok(None);
            }
            // A limit whose wall the transcript refused is an idle point, and
            // a limit a cancel left that answered a turn is a limit point
            // ([`Self::point_as_decided`]): its episode, its escalation.
            self.point_as_decided(&mut point);
            // A box the loop did not approve, a question the turn-end policy
            // does not take as an offer, and a point the policy escalates are
            // the manager's: escalated once ([`Self::escalate_point`]; a
            // stop-at-review seam does none of it). A limit's escalation is its
            // episode's ([`Self::limit_after`]); the memory wall read at a
            // limit notice escalates as the point's ([`Self::escalates_here`]).
            let point_escalated = review.unattended()
                && matches!(turn_end, TurnEndAction::Escalate { .. })
                && self.escalates_here(&point.phase);
            if review.unattended() {
                match (&point.phase, &turn_end) {
                    // A box held for a person at the keyboard is theirs,
                    // right there: nothing is raised.
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
            // The point's own line, and the task's end the fold took right
            // after it ([`Self::say_point`]).
            if !self.say_point(&point, review)? {
                return Ok(Some(End::Stopped(point)));
            }
            // The manager has the worker now, as after a fresh `supervise`.
            state.approved.clear();
            state.handed = Some(self.review_key(&point));
            state.handed_seq = Some(point.screen.seq);
            state.handed_submit = submit_mark(&point.screen);
            if review.unattended() {
                // A wall the policy acts on, or waits out, raises nothing:
                // nobody is needed there. Nor does one whose point raised
                // the memory wall's escalation: the one badge is the
                // person's restart, not the limit's over it.
                let handled = point_escalated
                    || !matches!(
                        turn_end,
                        TurnEndAction::Nothing | TurnEndAction::Escalate { .. }
                    );
                self.limit_after(&point, deadline, handled, review)?;
            }
            // The host's step goes first at an idle point it asked for (the
            // live upgrade, a relaunched agent's continuation): the point's
            // own act is not taken on a screen the step has moved.
            if self.host_steps_here(&point, opts, &mut false, review)? {
                // The host took the point: a probe the roster said was due
                // here is not run, and says so.
                self.probe_shut(&point, ProbeGate::HostWants, None, review)?;
                state.gone_first = false;
                return Ok(None);
            }
            if review.unattended() {
                // The accounts probe, where the host claims one for this point
                // ([`Self::usage_probe`]): a step of the loop's own between the host's step and
                // the point's act, moving none of the policy's memory. With no claim it is `No`
                // before a byte is written, and the act goes on as decided.
                match self.usage_probe(&point, &turn_end, opts, review)? {
                    Probed::No => self.turn_end_execute(&point, turn_end, opts, allow, review)?,
                    // A re-decision is set: the wait below ends on it.
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

    /// The session survey at one look ([`super::phase::survey_open`]). When it appears
    /// ([`Survey::Closed`]) the loop says `EVENT survey …` with the guarded key that dismisses it
    /// ([`survey_event_line`]), once per appearance.
    ///
    /// Under `dismiss_surveys` the loop presses that `0` itself
    /// ([`Self::dismiss_survey`]) and `true` has it look again from a fresh
    /// read, where the `0` is judged ([`Survey::Tried`]): the survey gone,
    /// the loop says `DISMISSED survey seq=<n>` (the press's stamp), or
    /// nothing when the press was skipped; the survey
    /// still open — the `0` did not take, or the guard matched no row of it —
    /// is TRIED AGAIN on a growing back-off ([`SURVEY_RETRY`],
    /// [`Self::retry_survey`]): the `EVENT survey` line said at the first
    /// failure, the host told from the [`SURVEY_BADGE_AT`]-th (a record; it ends
    /// with the survey), the tries going on; a `0`
    /// that landed in the composer instead is backspaced, nothing is said
    /// dismissed, and a survey still open then is tried again the
    /// same way. The survey gone over text a person typed that BEGINS with the
    /// `0` (`0hello`, `00`: their key landed in the ~0.44 s the card takes to
    /// take the digit, which abandons the card) was not dismissed: the journal
    /// says `ABANDONED seq=<n> …`, and the text is theirs
    /// — the `0` is not taken out of it. A person who keyed between the read
    /// and the press skips the `0` altogether (`if-human=`, [`Dismiss::Yielded`]):
    /// no try is spent, and their grace holds the card at the next look.
    ///
    /// Under `dismiss_surveys` the loop presses that `0` itself ([`Self::dismiss_survey`]), and
    /// `true` has it look again from a fresh read, where the `0` is judged ([`Survey::Tried`]):
    /// gone, it says `DISMISSED survey seq=<n>`; still open, it is retried on a growing back-off
    /// ([`Self::retry_survey`]). A `0` that landed in the composer is backspaced; the survey gone
    /// under a person's text that begins with the `0` is `ABANDONED`, the text left theirs.
    /// Nothing is pressed while a box is up or a draft is typed (a `0` would land in the draft)
    /// or within a person's grace. A press with no answer is ridden out with the survey not yet
    /// tried.
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
        // The survey the last look left is gone if any read since showed
        // none open (this turn's read among them).
        let left = std::mem::take(&mut self.survey_gone);
        let lone_zero = stray_digit(reader, &turn.screen, "0");
        // A `0` of ours that the composer no longer holds, once the card has had its time
        // to take it, is accounted for: taken, or lost. (Under a box the composer is not
        // shown, and the `0` stays owed.)
        if !lone_zero
            && turn.phase != Phase::Prompt
            && self
                .survey_zero
                .is_some_and(|at| self.fx.since(at) >= self.setup.survey_land)
        {
            self.survey_zero = None;
        }
        // While the card is open, a `0` of ours younger than [`SURVEY_LAND`] is neither judged
        // nor removed (a backspace would cancel our own pending dismissal); the wait is the
        // card leaving.
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
        // A `0` of ours standing alone in the composer past that time was dropped by the card
        // (it had mounted again, or the hook was not armed): backspaced, whatever the machine's
        // state. Only an `OK` removes it: a refusal parks or backs the loop off and keeps the
        // `0` owed.
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
        // A draft is judged where it is drawn ([`Self::composer_draft`]: the cursor's column and
        // the cell's dim, never its row), as a turn end's act judges it: a person's draft whose
        // caret was moved home sits at column 2 like the placeholder.
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
                    // A person's key landed in the ~0.44 s the card spends taking the `0`: the
                    // card is abandoned, not dismissed, and the `0` heads what they typed
                    // (`0hello`, `00`). Said as it is; their text, `0` included, is theirs.
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
            // Still open under a box or a draft: judged at the first look
            // without them.
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
            // A busy read presses a card once per appearance: Claude Code's digit hook arms on
            // the card's own mount, not the turn, so a `0` that did not take gets no second one
            // from a busy read. The idle look at the turn's end carries the ladder.
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
            // A person typing may be rating it: nothing is said or pressed
            // until their grace has passed.
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
                // A person keyed between the read and the press: nothing was written and
                // no try is spent; the next look reads again and holds for their grace.
                Dismiss::Yielded => {
                    self.survey_gone |= left;
                    return Ok(true);
                }
                // Nothing written: the survey stays as it was, tried again
                // once the hold lifts or the back-off passes.
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
        // A card the harness will not press is answered by nobody unless someone is told:
        // nothing else acts at a point with a survey open. The badge goes with the point.
        if let Some(why) = withheld
            && !busy
        {
            self.tell_withheld_survey(turn, why, review)?;
        }
        Ok(false)
    }

    /// A rating card the harness will not press is open: told to the host once per appearance;
    /// it ends with the card ([`Self::survey_ended`]). No tab is marked: a badge says the owner
    /// is needed now, and this card blocks nothing the worker does.
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

    /// Say a survey the loop cannot close, once (`kind` at a time): the journal's
    /// `WAITING` line, and the host's [`IdleHost::survey_stuck`].
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

    /// No survey is on the screen at the first look of this loop that tells the
    /// host anything about surveys: the host is told once that there is none
    /// ([`IdleHost::survey_stuck`] with `None`). A fact the host holds from an
    /// earlier loop or process ends here even though this loop never saw the card
    /// leave; the host's end is idempotent.
    fn survey_absent(&mut self, review: &mut dyn Review) {
        if self.survey_absent_told || self.survey_told.is_some() || !review.unattended() {
            return;
        }
        self.survey_absent_told = true;
        if let Some(host) = &self.stall_host {
            host.survey_stuck(None);
        }
    }

    /// The survey has left: the host is told it ended, and the journal says
    /// `CLEARED`.
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

    /// The wait for the card to take the `0` the harness wrote (`wait` is what is left of
    /// [`SURVEY_LAND`]): the card leaving the screen ends it — `await gone` on its own
    /// question row.
    fn survey_pending(
        &mut self,
        block: &aterm_phase::SurveyBlock,
        seq: u64,
        wait: Duration,
    ) -> Result<(), Fail> {
        self.wait_row_gone(&block.guard, seq, wait)
    }

    /// ONE WAIT FOR A ROW TO LEAVE THE SCREEN, `wait` at most: `await gone
    /// <guard>` — level-triggered, so it ends the moment no row matches —
    /// and, with no guard to name, the screen's next move past `seq`. The rating card's own question row
    /// ([`Self::survey_pending`]) and the Settings panel's header row
    /// ([`Self::usage_panel_look`], [`Self::panel_escape`]) are waited on so:
    /// a person who closes either ends the wait at once.
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

    /// How much longer the rating card now open must show before its `0` is pressed
    /// ([`SURVEY_SETTLE`]), counted from the read that first showed it — `None` once
    /// it has shown that long. The card is remembered until a read shows it gone
    /// ([`Self::screen`]), so a card held for a person waits no more.
    fn survey_young(&mut self) -> Option<Duration> {
        let first = *self.survey_first_seen.get_or_insert_with(|| self.fx.mono());
        let wait = self
            .setup
            .survey_settle
            .saturating_sub(self.fx.since(first));
        (!wait.is_zero()).then_some(wait)
    }

    /// Whether a person holds the survey's act (`act`): [`Self::held_for_person`] at an
    /// idle look, [`Self::survey_held_busy`] on a busy read.
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
            self.held_for_person(seq, act, true, opts, review)
        }
    }

    /// [`Self::held_for_person`] for the reads of a busy wait: the same test (a person typed
    /// within `human_grace_s` of a status read made now), but no `turn_end_due` and no line per
    /// read: the hold is remembered until the grace ends, and noted once. A busy read has no
    /// point to look at again, and its next read comes with the screen.
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

    /// The journal's `HELD seq=<n> <who>: hands off until <UTC> (<act>)` line, as
    /// [`Self::held_for_person`] writes it, for a hold that is not a look's own: the survey's `0`
    /// on a busy read and the accounts probe's Escape, each said once a hold.
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

    /// The survey's `0` did not dismiss it (`failed` tries before this one): tried again after
    /// [`SURVEY_RETRY`]'s pause, the loop looking again then ([`Self::look_at`]). The first
    /// failure says the `EVENT survey` line (a watching manager's cue); from the
    /// [`SURVEY_BADGE_AT`]-th the host is told ([`IdleHost::survey_stuck`]; never a tab mark)
    /// while the tries go on, never handed over for good.
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
        // Only the idle look re-decides at `at`; a busy read's next read finds the retry due.
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
        // A badge says the survey blocks the worker's continuation, which only an idle
        // point does; a busy read raises none (the next busy read would clear it).
        if tries == SURVEY_BADGE_AT && !ctx.busy {
            let why = format!("the session survey did not close ({tries} tries); still trying");
            self.survey_stuck(turn.screen.seq, StuckSurvey::Unclosed, &why, review);
        }
        Ok(())
    }

    /// Ride out an outage: a request the server did not serve ([`Self::unserved`]), as when a
    /// self-update hands every session to a new instance under the same `@sid`. Reads the screen
    /// of the same selector after a pause starting at 0.5 s and doubling up to 8 s across the
    /// outage, until a read answers (`Rode::Back`) or the window runs out. The client resolves
    /// its socket afresh each request: unnamed, the instance hosting the caller's terminal, else
    /// the newest (`aterm.sock`); a named per-instance socket (`aterm-<pid>.sock`) goes with its
    /// instance.
    ///
    /// The window is the outage's, from the first unserved request until the loop gets past it;
    /// a probe answering only says the session answers a read, so a later unserved request is
    /// the same outage. `RECONNECT <why>` (cut at 160 characters) is said at the first ride-out
    /// and `RECONNECTED after <ms> ms` at the first answer.
    ///
    /// Ends the loop (`Err`) when the window runs out (`reconnect window lapsed: <the last
    /// failure>`), at once when the window is zero, and on a probe `ERR` that
    /// [`Self::unserved`] does not name; budget spent first is `Rode::Spent`. The probe is a
    /// read, never the failed request: a new instance's content seq starts over, and a press is
    /// decided again from a fresh read. Probed caps are forgotten first: the answering instance
    /// may be another build.
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

    /// Wait (one step, capped by `deadline` and `step_cap`) for the content to move past `seq`.
    /// A step that runs out is checked with a read: `await seq <n>` latches only once this
    /// instance's count passes `n`, so after a handoff in which no request was seen to fail the
    /// loop could wait on a count the successor (whose count started over) may not reach for
    /// hours. A read below `seq` is that other count: the screen moved. Nothing is read once
    /// the budget is spent.
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

/// Tests: a hand on a session's fixed clock ([`Session::clock_jump_handle`]).
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct ClockJump(Arc<super::fx::ScriptedClock>);

#[cfg(test)]
impl ClockJump {
    /// The Mac slept `secs` NOW: the fixed clock's next read and every one
    /// after it is that much later, the monotonic clock standing still
    /// ([`super::fx::ScriptedClock::slept`]).
    pub(crate) fn slept(&self, secs: i64) {
        self.0.slept(secs);
    }
}

/// The limit episode: `watch`'s alone (see the module doc). Every request
/// here goes through [`Session::call`] like the loop's own, so an outage
/// under any of them is ridden out the same way.
impl<C: Ctl> Session<'_, C> {
    /// Unix seconds now: the fixed clock in tests, else the system's
    /// ([`Fx::wall_s`]).
    fn now_unix(&self) -> i64 {
        self.fx.wall_s()
    }

    /// Unix MILLISECONDS now: the clock the loop turns a unix time into an
    /// `Instant` by, and back ([`Self::instant_of`], [`Self::unix_of`]). The
    /// fixed clock in tests (whole seconds, plus the phase
    /// [`Self::set_clock_at_ms`] gives it), else the system's ([`Fx::wall_ms`]).
    ///
    /// NOT WHOLE SECONDS: converted against [`Self::now_unix`], which
    /// truncates, a reset placed at `U` would become an `Instant` carrying the
    /// sub-second phase of the read that made it while its journal stamp drops
    /// the phase of the read that prints it; whenever a second turns between
    /// the two, one placement is journaled as two `until=`s a second apart.
    fn now_unix_ms(&self) -> i64 {
        self.fx.wall_ms()
    }

    /// The local zone's offset, read from `date` once — through the loop's
    /// door (`probe.tz_offset` on a harness tape).
    fn local_offset_s(&mut self) -> i64 {
        if let Some(o) = self.local_offset {
            return o;
        }
        let o = self.fx.tz_offset(self.local_offset_fixed);
        self.local_offset = Some(o);
        o
    }

    /// A zone's offset by name at an instant, as the limit readers take it
    /// (`&dyn Fn(&str, i64) -> Option<i64>`): the session's reader
    /// ([`Self::zone_offset`]) asked through the loop's door, so each answer
    /// a reset's placement turned on is a `probe.zone_offset` on a harness
    /// tape. The one reader of the field.
    fn zone_lookup(&self) -> impl Fn(&str, i64) -> Option<i64> + '_ {
        let live = self.zone_offset;
        move |zone, unix| self.fx.zone_offset(live, zone, unix)
    }

    /// The local zone's offset at `unix` ([`Self::local_offset_at`]), asked
    /// through the loop's door (`probe.local_offset_at`). The one reader of
    /// the field.
    pub(super) fn local_offset_then(&self, unix: i64) -> Option<i64> {
        self.fx.local_offset_at(self.local_offset_at, unix)
    }

    /// The reset a notice names as Unix seconds; `None` when it names none the
    /// watcher can read. The reset the loop PLACED for this wall
    /// ([`Self::wall_reset_at`]: [`Self::reset_unix_for`], by the wall's kind and
    /// print) when it placed one for these words — the ONE placement, so the
    /// budget stretched past the reset (`EXTEND`) and the wait for it agree —
    /// else [`limit::reset_at`]'s, the session window's rule, for a bare clock time
    /// on the day the agent's transcript names ([`Self::transcript_day`]).
    fn reset_unix(&mut self, reset: Option<&str>) -> Option<i64> {
        let text = reset.unwrap_or("-");
        let placed = self
            .wall_reset
            .as_ref()
            .filter(|(key, _)| key.ends_with(&format!("|{text}")))
            .and_then(|(_, unix)| *unix);
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

    /// A bare clock time names no day: `resets 3pm` read by a loop started on a
    /// later day than the notice (an update, a reaped worker, a notice standing
    /// overnight) would be placed on the read's day ([`limit::reset_at`]), a
    /// day or more late. Where the words name no date and the transcript's
    /// reset ([`IdleHost::limit_until`], Claude Code's `quotaLimits.resetsAt`)
    /// is that same clock time (same minute) a whole number of days before
    /// `at`, it is the reset. Anything else is `at`: a dated reset names its
    /// own day, and a transcript naming another time is not this notice's.
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

    /// [`Self::reset_unix`] for a wall read on `rows`.
    ///
    /// A session window (`session_window`: the notice says `session limit` or `5-hour`; a
    /// `Usage limit reached · …` naming no window may be weekly) resets within five hours, so a
    /// bare clock time on one is [`limit::reset_at`]'s: today's, or tomorrow's only when that
    /// is under five hours off. Any other wall's bare clock time is printed only for a reset
    /// within 24 hours of the print (the date beyond that), so the reset is the first
    /// occurrence of that clock after the print ([`limit::place_reset`]). The print is what the
    /// screen says: the done row's clock (`done 12:19 PM`, its last occurrence within a day, at
    /// the start of its minute), else, for a wall the loop saw arrive (`live`),
    /// [`limit::PRINT_LAG_S`] ago, less [`limit::CLOCK_SLACK_S`] either way. A wall found
    /// sitting with no done row has no evidence of its print: the session rule places it, and a
    /// wrong guess costs one try, since the wall answers it and that is a print the loop sees
    /// ([`Self::wall_reset_at`]). Either way a bare clock time is then read against the agent's
    /// transcript ([`Self::transcript_day`]).
    ///
    /// `answered`: the wall stands where an act of the loop's went untaken
    /// ([`TurnEndState::untaken_act`]), so the print is no earlier than [`limit::PRINT_LAG_S`]
    /// ago, whatever done row stands above the composer.
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

    /// The notice's reset read again — the notice printed again after the
    /// episode opened (a retry's, or the answer to a continuation,
    /// [`Self::limit_after`]). The same text is the same reset and is not
    /// re-read: a span (`in 3h`) counts from the print, and re-reading it would
    /// push the reset out each time. Other text is read, and a later reset it
    /// names is the one waited for, with `EXTEND` owed again.
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

    /// After a point was reported. A limited point opens the episode
    /// ([`Self::open_episode`]; none when the turn-end policy acts on the wall
    /// at once, `handled`), or with the episode open refreshes its reset
    /// ([`Self::refresh_reset`]) and escalates it if it never was and the
    /// policy now waits on the wall; then the budget is stretched past the
    /// reset ([`Self::extend`]).
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
                // Opened on a wall the policy acted on at once, and waiting
                // on it now (the wall again after the act): escalated now.
                if !handled && !raised {
                    // The notice names its own reset; the journal keeps it apart.
                    // Whether this loop waits it out is said before the
                    // badge goes up (`kind=`), by the reset the episode
                    // holds now.
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

    /// A limited point with no episode open: the episode opens on it. A new one
    /// is escalated ([`Self::escalate`]) unless the turn-end policy acts on the
    /// wall at once (`handled`: a model switch), journaled `LIMITED seq=<n>
    /// handled: <message>`. One a busy read (or an Off park) closed and the
    /// worker has not answered since ([`Retry`]) is the same episode
    /// reopened: attention set again, no second ask (`again=the retry …`),
    /// its seq carried, the reset the notice names now.
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
        // Read BEFORE the badge goes up, which says it (`kind=`).
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

    /// Whether the limit a notice of `message` names is waited out with no
    /// one's act, so its badge is a wait (`kind=wait`), not an ask: Claude
    /// Code goes on by itself ([`limit::resumes_by_itself`]) — whatever the
    /// owner's policy, since the loop closes that episode on the busy read
    /// ([`Self::limit_on_busy`]) and types nothing — or the owner's policy
    /// resumes limits (`[harness] resume_limits`) and the notice names a
    /// reset this loop read (`reset_unix`), no further off than
    /// [`RESET_HORIZON`] (one further is misread), which the turn-end policy
    /// continues after. A limit naming no reset it can read is waited out by
    /// nobody here — a reset is never guessed — and is an ask.
    pub(super) fn waits_out(&self, message: &str, reset_unix: Option<i64>) -> bool {
        let horizon = i64::try_from(RESET_HORIZON.as_secs()).unwrap_or(i64::MAX);
        limit::resumes_by_itself(message)
            || (self.resume_limits && reset_unix.is_some_and(|at| at - self.now_unix() <= horizon))
    }

    /// A busy read inside the wait for a turn ([`Self::await_turn_from`]) with an episode open
    /// whose notice said Claude Code would go on by itself ([`Episode::resumes_itself`]): the
    /// episode closes now (`CLEARED seq=<n> … the worker works again`, attention unset), not at
    /// the next review point, which the resumed turn may keep hours off. An episode whose notice
    /// named a reset is untouched. The closed episode is kept ([`Retry`]) until the worker
    /// answers on a point that is not the notice, so a retry that hits the wall again reopens it
    /// with no second mail ([`Self::open_episode`]).
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

    /// The budget stretched past the reset the notice named: a deadline that
    /// would fall before it plus [`RESET_GRACE`] becomes that, and `EXTEND
    /// until=<UTC> reset=<text>` is said — once a reset (a later reset in a
    /// retry's notice is looked at again). A budget already past it, a
    /// notice naming no reset, or one read as further off than
    /// [`RESET_HORIZON`] (no limit names such a reset: it is misread)
    /// changes nothing.
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

    /// The wait for the screen to move past `seen`, step after step until the budget, bounded by
    /// the turn-end policy's `WaitUntil` ([`Self::turn_end_due`]), when the policy decides again
    /// on the point still showing ([`Self::turn_end_now`]). A screen change ends the wait first.
    /// `Some(End::Timeout)` when the budget runs out; `None` otherwise, including after the
    /// host's step at an idle point ([`Self::host_steps_here`]).
    ///
    /// What ends a step is the cheapest thing that can: a box handed over, its
    /// own row LEAVING (Claude Code blinks the tool row above an open box twice
    /// a second, and every blink would be a look at the same box), or — for a
    /// FILE box, whose first row (its path) two requests share, that row
    /// standing a whole step — one read showing another REQUEST under it
    /// ([`file_request_key`]: its title to its question, never its options, a
    /// person's ticks or typing, its countdown or the transcript above it, so
    /// someone answering the box, or the main thread printing over a
    /// subagent's, ends nothing); an
    /// idle point or a question with nothing of the loop's in flight, the
    /// server's own agent verdict MOVING off it ([`Self::agent_wake`]: one
    /// parked request, pushed by the server, no screen read — a footer's clock
    /// or a person's draft wakes nothing) — save while the agent is STARTING
    /// (no mode and no busy frame read yet), when the frame that draws its
    /// footer must be read and the content is waited on, and while a draft
    /// holds the point ([`Self::draft_held`]); anything else, the
    /// content moving (`await seq`).
    fn wait_for_next(
        &mut self,
        seen: Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        deadline: &mut Instant,
        review: &mut dyn Review,
    ) -> Result<Option<End>, Fail> {
        let anchor = (seen.phase == Phase::Prompt)
            .then(|| box_anchor(self.reader(&seen.screen.rows), &seen.screen.rows))
            .flatten();
        // A stall a read since the look reported is held before the first
        // step, its badge withdrawn now (`stall.rs`).
        self.stall_before_wait(seen.screen.seq, review)?;
        // A question dialog the policy holds for the person is waited on in
        // short steps, the session's word re-read after each: handed back, it
        // is looked at again at once ([`Self::question_policy_answers`]).
        let held = self.question_held
            && aterm_phase::parse_prompt_v2(&seen.screen.rows)
                .is_some_and(|p| p.kind == aterm_phase::PromptKind::Question);
        let step_cap = if held {
            HANDBACK_POLL
        } else {
            self.setup.wait_step
        };
        // A Mac that slept through the wait: `Instant` does not count sleep and the wall clock
        // does, so the policy's deadline falls at its wall time all the same
        // ([`Self::due_at`]); when the wall clock has run ahead by more than a minute, a deadline
        // the sleep carried past is decided again after the wake's settle
        // ([`Self::check_slept`], [`Self::woke`]). The clocks are read on entering the wait too.
        self.watch_sleep();
        // The review key of `seen`, built at most once for the whole wait
        // ([`Self::agent_wake`]).
        let mut seen_key: Option<String> = None;
        // The content's count the verdict's wait last asked from
        // ([`Self::moved_under_verdict`]): `seen`'s, then each read since.
        let mut verdict_seq = seen.screen.seq;
        // What the file box `seen` shows asks, for the read after its first row stood a whole
        // step; `None` for every other point, a question held for the person included.
        let request = anchor
            .as_ref()
            .filter(|_| !held)
            .and_then(|_| self.file_request(&seen));
        // The host's quiet point is asked first in this wait as at the point
        // — the point's own act typed, its `DONE` said — and at every later
        // wake as a wake ([`IdleHost::at_withheld_point`]). FIRST ASKED, not
        // first woken (F1-R5-A1): a host that wanted no point as the wait
        // began (its idle step's look not due yet) is asked first, once that
        // look comes due, as at a point.
        let mut asked = false;
        loop {
            if self.stopped() {
                return Ok(None);
            }
            self.check_slept(seen.screen.seq, review);
            self.dialog_note(&seen.screen, review);
            if self.host_steps_here(&seen, opts, &mut asked, review)? {
                return Ok(None);
            }
            // The host let the point go (its step owns nothing, its ownership
            // lapsed, `[harness] upgrade` switched off): the point is decided
            // again now, as at its deadline.
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
            // The API back: at a network wall, the host's measure turning up
            // decides the point again within this step, not at the ladder's
            // next rung. Any other new measure decides again too (a Down lost
            // to Unknown returns to the ladder). Asked at the top of the step
            // so a step that latched on a screen tick skips nothing.
            if self.reach_edge(opts) {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            // The memory ceiling while the loop waits: past it, a warning; at an idle point
            // decided without the Mac's trouble, a measure acted on decides the point again
            // now, and one no longer acted on hands back the act the memory took
            // (`memory_edge`).
            if self.memory_edge(&seen, opts, review)? {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            // The accounts moved under a wait (the owner pinned one or renewed a login, a login
            // was found signed in, another session proved or struck one), which moves nothing on
            // this screen: the host's stamp says so, and the point is decided again
            // (`account_edge`).
            if self.account_edge(opts) {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            // A look owed at the point still showing (a survey's `0` to try
            // again): the wait ends there, and the loop looks again.
            if self.look_at.is_some_and(|at| self.fx.mono() >= at) {
                self.look_at = None;
                return Ok(None);
            }
            self.tell_wait(opts);
            // The policy's deadline read against both clocks at every step:
            // a wall clock a sleep carried past it ends the wait now.
            let due = self.due_at().into_iter().chain(self.look_at).min();
            let until = due.map_or(*deadline, |t| t.min(*deadline));
            // A look the host scheduled ends this step, not the wait: the
            // step's wake reads [`IdleHost::wants`] again at the top. One
            // already past (the host wants a point this one cannot be) keeps
            // the ordinary step, never a spin.
            let now = self.fx.mono();
            let step_cap = opts
                .idle_host
                .as_ref()
                .and_then(|h| h.wants_at())
                .filter(|at| *at > now)
                .map_or(step_cap, |at| step_cap.min(at - now));
            // …and so does the wake's settle falling: what it withheld (the
            // host's point) is offered at the top again.
            let step_cap = self.settling().map_or(step_cap, |at| {
                step_cap.min(at.saturating_duration_since(now))
            });
            if now < until {
                let step = until.saturating_duration_since(now).min(step_cap);
                // A step of the wait runs: after it, whatever ends it, a point
                // the host passed over is its again ([`Self::host_passed`]),
                // its follow-ups' count included ([`Self::host_follows`]).
                self.host_passed = None;
                self.host_follows = None;
                if let Some(anchor) = anchor.as_deref() {
                    if self.await_gone(&["gone", anchor], step)? {
                        return Ok(None);
                    }
                    // The box's first row stood the whole step. A FILE box
                    // REPLACED under that row keeps it — a second Edit on the
                    // same path — and the wait never latches on it: one read
                    // says whether the request showing is still this one, and
                    // another is the next look's. What is compared is the
                    // REQUEST, never the box's review key: that key keeps a
                    // question's ticks, its typed text and a preview that
                    // follows the focus, and the letters above the box, so a
                    // person (or the manager) answering it, or a row the main
                    // thread prints over a subagent's box, would read as the
                    // box answered and asked again every step (measured: two
                    // asks for one ticked checkbox, on a held question's 2 s
                    // steps). Every other box is read again only when its row
                    // leaves.
                    if let Some(request) = request.as_deref() {
                        let screen = self.screen()?;
                        let now = self.turn_of(screen);
                        if self.file_request(&now).as_deref() != Some(request) {
                            return Ok(None);
                        }
                    }
                } else if let Some(words) = self.agent_wake(&seen, &mut seen_key) {
                    // A whole recheck's step is asked of the content after
                    // it; a shorter one ends at the point's due, whose
                    // decision reads the screen afresh and acts on no other
                    // point ([`Self::turn_end_now`]). A whole step is the
                    // recheck's, or the setup's own where that is shorter
                    // (a development build's settle seam: every step 2 s,
                    // none ever a whole 5 s, and the recheck never ran —
                    // the review of `b9/forever-gaps`). A shipped step
                    // (20 s) asks exactly as before.
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
                // A step that did not latch: is the worker reading its input
                // (`stall.rs`)? A held stall waits past the policy's due too.
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

    /// Tell the host the loop's next try ([`IdleHost::waiting`]): the deadline
    /// of the policy's wait when it moved since last told, as the unix second
    /// it falls at ([`Due::unix_ms`]: a sleep does not move it). No wait is
    /// told once as `None`, so a tab drops the last try's time. The same
    /// `Instant` set again at a wall second more than [`WAIT_SKEW_S`] from the
    /// one told is told again.
    fn tell_wait(&mut self, opts: &SuperviseOpts) {
        #[cfg(test)]
        if TELL_EVERY_STEP.with(std::cell::Cell::get) {
            self.wait_told = None;
        }
        // Only a wall's retry wait (the next try) and the short streak's
        // back-off (a pause) are told ([`Due::wait`], [`WaitKind::told`]):
        // the check that the worker took the try just typed is told as no
        // plan.
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

    /// THE SESSION'S SLEEP WATCH: the two clocks read against the marks the
    /// last read left ([`Self::clock_marks`]). When the wall clock has run
    /// ahead of the monotonic one by more than a minute ([`slept_through`])
    /// the Mac may have slept, and the machine's own count of its sleep
    /// ([`Self::recount_sleep`]) says how long, where it keeps one (a wall
    /// clock stepped forward is no sleep; with no count the clocks' lead is
    /// the sleep, as before the count was read). A sleep past a minute is the
    /// session's AT ONCE: written in its book between the two reads
    /// ([`Self::sleeps`], which any turn's done row is read against:
    /// [`Self::turn_asleep`]), the policy's pauses and kept resets moved by
    /// it ([`TurnEndState::slept`]) before anything is stamped on the wake's
    /// side of it, and the wake's settle placed from THIS read
    /// ([`Self::settle_wake`]) — whatever reaches the loop first after it,
    /// nothing typed into the agent goes before the settle falls
    /// ([`Self::settling`]). A due the sleep carried past is moved to the
    /// settle at the next place that can say so ([`Self::check_slept`]). Read
    /// after every request ([`Self::call`]: a sleep spans one, whatever
    /// answers it — a busy turn's reads among them), at each step of a wait,
    /// and where a decision takes its `now`. The marks move to now either
    /// way. A clock stepped BACK is no sleep. The first read is the loop's
    /// first look at the machine's sleep ([`Self::look_at_sleep`]).
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

    /// THE MACHINE'S COUNT OF ITS SLEEP read again: how long it slept since
    /// the loop last read the count ([`Self::sleep_count`]); `None` where it
    /// keeps none (then, or now). The count read is the one the next is set
    /// against.
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

    /// THE LOOP'S FIRST LOOK AT THE MACHINE'S SLEEP, at `now_ms`, once: its
    /// count, which its watch sets a later count against
    /// ([`Self::recount_sleep`]), and the system's record of its last sleep,
    /// written in the book where it ended before this look — a sleep no read
    /// of this loop's measured ([`SleepBook::place_last`]) — and its wake's
    /// settle placed from the wake it records ([`Self::settle_wake`]), as
    /// the loop that measured it placed it: one that falls after this look
    /// holds what this loop types until then, as it held that loop's. The
    /// ledger's part, the sleep since the last act ([`Self::seed_sleep`]),
    /// comes with the seed, set against this look.
    fn look_at_sleep(&mut self, now_ms: i64) {
        if self.sleep_look.is_some() {
            return;
        }
        let count = self.fx.asleep();
        let last = self.fx.last_sleep();
        // A wake further back than any settle reaches holds nothing.
        let reach = turn_end_loop::WAKE_SETTLE + turn_end_loop::WAKE_SPREAD;
        if self.sleeps.place_last(now_ms, last)
            && let Some((_, woke_ms)) = last
            && Duration::from_millis(u64::try_from(now_ms - woke_ms).unwrap_or(0)) < reach
        {
            let woke = self.instant_of_ms(woke_ms);
            self.settle_wake(woke_ms.div_euclid(1000), woke);
            // One that fell before this look is let go.
            let _ = self.settling();
        }
        self.sleep_count.clone_from(&count);
        self.sleep_look = Some(FirstLook {
            at_ms: now_ms,
            count,
            last,
        });
    }

    /// THE SLEEP SINCE THE LAST ACT ON THE BOOKS, which no read of this loop's
    /// measured (`body`, the ledger text the seed read, at `now_ms`): the
    /// machine slept the difference between the count the act's row records
    /// and this loop's own at its first look, on the same boot — the
    /// system's last sleep where it fell since the act, and the rest between
    /// the act and that sleep ([`SleepBook::count_since_act`]). Where the row
    /// holds no count (an older build wrote it), or the count is another
    /// boot's, nothing is written: the act's turn's done row reads as
    /// before, but for what the system's record places (data that does not
    /// exist; it ages out with those books), and the journal says so where it
    /// matters ([`Self::sleep_unknown`]).
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

    /// THE SLEEP THE BOOKS COULD NOT COUNT, journaled once
    /// ([`Self::sleep_unsaid`]): a turn's done row reaching back past this
    /// loop's first look was read as before the count, whole of any sleep
    /// before that look but the system's record of its last.
    fn say_sleep_unknown(&mut self, seq: u64, review: &mut dyn Review) {
        if let Some(why) = self.sleep_unsaid.take() {
            review.note(&format!(
                "SLEEP seq={seq} not counted before this loop's first look ({why}): the turn's \
                 done row is read whole of it, but for the system's record of its last sleep"
            ));
        }
    }

    /// The turn-end policy's `now` for a decision: read after the watch
    /// ([`Self::watch_sleep`]), so a sleep is the policy's before the decision
    /// stamps anything on the wake's side of it.
    fn policy_now(&mut self) -> Instant {
        self.watch_sleep();
        self.fx.mono()
    }

    /// The watch read ([`Self::watch_sleep`]), and a wake it measured handed
    /// to [`Self::woke`]: at each step of a wait, before its deadline is read.
    fn check_slept(&mut self, seq: u64, review: &mut dyn Review) {
        self.watch_sleep();
        if let Some(wall_now) = self.slept_at.take() {
            self.woke(seq, wall_now, review);
        }
    }

    /// The wait the host was told is over ([`IdleHost::waiting`]`(None)`): the policy decides
    /// the point again ([`Self::turn_end_now`]) or the worker works ([`Self::await_turn_from`]).
    /// Told where the wait ends, not at the next `wait_for_next`: after a try the loop sits in
    /// the worker's whole busy turn, where a plan left standing would be laid on the next wall.
    /// Nothing told, or told already: nothing.
    pub(super) fn end_told_wait(&mut self, opts: &SuperviseOpts) {
        if !matches!(self.wait_told, Some(Some(_))) {
            return;
        }
        self.wait_told = Some(None);
        if let Some(host) = opts.idle_host.as_ref() {
            host.waiting(None);
        }
    }

    /// A new measure from the host at a network wall: the cue to decide the point again at once
    /// ([`Self::wait_for_next`]). An `Up` continues within the step; a `Down` lost to `Unknown`
    /// returns the point to the unmeasured ladder; a new `Down` moves the wait out to the hold,
    /// as `SupervisorNetworkWall` assumes. Asked only while the point is a network wall
    /// ([`Self::reach_seen`]) and a policy wait stands ([`Self::turn_end_due`]): an escalated
    /// point decided again would post its ask again. A measure already seen (same `since`) is
    /// no edge.
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

    /// The host's advice about the session's account may have changed while the loop waits
    /// ([`Self::account_news`]): an edge (the point decided again at once) only where the
    /// standing decision asked the host ([`Self::account_seen`]) and a policy wait stands
    /// ([`Self::turn_end_due`]; deciding an escalated point again would post its ask again).
    /// Never under `[harness] accounts = "off"` with `logins` off too.
    ///
    /// A latched session (the host refused its move for good,
    /// [`TurnEndState::account_refused`]) hears the news too, so a remedy the owner applied
    /// reaches it ([`TurnEndState::account_news`]), but at most once per [`ACCOUNT_NEWS_EVERY`],
    /// so a host whose every refusal moves its stamp is not asked as fast as it refuses; news in
    /// between is read at the next step.
    ///
    /// A DECISION THAT READ THE ROSTER'S WORD HEARS THE NEWS TOO
    /// ([`Self::roster_seen`]: a weekly limit, or a `/login` left at one),
    /// though it asked the host for no advice: there the owner's notice may
    /// name a line of aterm.toml the host reads live and masks out of the
    /// loop's config (`relaunch = true`), whose edit hands no worker over and
    /// moves the stamp instead — and the point decided again asks the host
    /// for the switch that line lets it make, rather than waiting the week
    /// out under a notice naming a line already set (the review of the login
    /// carry, r5h-1). A session limit reads no roster word, and its wait
    /// hears none of it.
    ///
    /// A renewal made where the point asked for it is an edge with no policy wait standing
    /// ([`Self::renewal_made`]): the escalation that asked for it is stale once it is made.
    ///
    /// SO IS THE NEWS AT A POINT THAT READ THE ROSTER'S WORD AND DID NOTHING
    /// ([`Self::nothing_decided`]): a `/login` left at a weekly limit under
    /// `resume_limits = false` sets no wait (nothing goes on past the reset),
    /// and its notice asks for another account (`lp`). Heard only under a
    /// wait, the account added and signed in reached nothing: the notice
    /// kept asking for a step already taken, and the line of aterm.toml the
    /// tab then needed was never named (the review of the login carry,
    /// r9h-1). Decided again, a point that did nothing posts nothing twice,
    /// and the notice is the one the roster's news makes true.
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

    /// The renewal the point asked for is made: the standing point was decided on a lost login
    /// whose process held its row's own login ([`Self::login_seen`]), so its step was the
    /// renewal, and where nothing waits at that wall (no relaunch to follow, a draft in the
    /// composer) no other edge decides it again. Once the host's stamp moves past the one read
    /// with that answer, the host is asked again (once per move, never every step), and a login
    /// now superseded decides the point again (`true`), so its badge and exception stop asking
    /// for the renewal. News of anything else keeps the point as it stands.
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

    /// The memory ceiling between points ([`IdleHost::memory`]), asked at the top of each step
    /// of a wait. Past the ceiling it is a warning ([`Self::memory_warn`]). A measure acted on
    /// ([`super::memory::MemoryOver::acts`]) at an idle point or limit notice the last reading
    /// judged without one ([`Self::memory_seen`] `None`) is an edge (`true`): the point is
    /// decided again at once with the memory wall, so an idle agent is not left until a turn
    /// that may never come.
    ///
    /// The down edge: a point whose act the memory took ([`Self::memory_over_wall`], or its
    /// escalation, [`BoxAsk::memory`]) is handed back once the measure is no longer acted on,
    /// its badge taken down (`CLEARED seq=<n> memory attention=<said>`, only if still ours), and
    /// decided again by its own rule.
    fn memory_edge(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let over = self.memory_measure(opts);
        // A restart refused stands through the measure's dips
        // ([`Self::memory_refused`]): the point handed back below is decided
        // again, and asks the host only if the memory acts there.
        if let Some(over) = over {
            self.memory_warn(&seen.screen, over, opts, review);
        }
        // BESIDE A PROVEN SELF-CLAIM no point is decided: the restart is
        // made at the agent's own idle point, read from its record
        // (`beside.rs`), never the screen's.
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

    /// The badge the last escalation raised ([`Self::escalate_point`]) is
    /// the memory measure's ([`BoxAsk::memory`]): handed back by
    /// [`Self::memory_edge`] when the measure is no longer acted on.
    pub(super) fn mark_memory_badge(&mut self) {
        if let Some(b) = self.box_ask.as_mut() {
            b.memory = true;
        }
    }

    /// The host's measure of the agent's memory judged against `[harness]
    /// memory_ceiling`: `Some` past it (a warning; acted on only where
    /// [`super::memory::MemoryOver::acts`]). A measure an eighth under the
    /// ceiling (or none) ends the warning's episode ([`Self::memory_told`]).
    fn memory_measure(&mut self, opts: &SuperviseOpts) -> Option<super::memory::MemoryOver> {
        let cfg = opts.policy.memory_ceiling;
        let m = opts.idle_host.as_ref().and_then(|h| h.memory());
        // A restart refused stands past the episode's end
        // ([`Self::memory_refused`]): a measure back over the ceiling is said
        // afresh with it.
        if super::memory::episode_over(cfg, m.as_ref()) {
            self.memory_told = None;
        }
        m.and_then(|m| super::memory::judge(cfg, &m))
    }

    /// The memory ceiling's warning: nothing is ended or held back; the measure is journaled
    /// `MEMORY seq=<n> …` and said to the host ([`IdleHost::warn`], keyed per tab) once an
    /// episode and whenever its verdict of what comes next changes ([`super::memory::Verdict`]:
    /// nothing, aterm's restart at the next idle point, or the person's). Said from the waits and
    /// before the host is asked for the restart: the point's own reading may be the first to
    /// find the measure acted on.
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
        // Where the policy asks for a restart: a host that makes one, `[harness] relaunch` on
        // (`memory_restart` escalates with it off), and not while the host's last word on one
        // is a refusal ([`Self::memory_refused`]).
        // Beside a proven self-claim the restart is the explicit one, its
        // exit relaunched by the host's crash rule (`beside.rs`).
        let makes = if self.beside_self_claim() {
            host.relaunches_after_stall()
        } else {
            host.can_restart()
        };
        let restarts = makes && opts.policy.relaunch && !self.memory_refused;
        self.memory_say(screen, over, restarts, opts, review);
    }

    /// [`Self::memory_warn`]'s row with `restarts` given: `true` once the host has made the
    /// restart (`restart_agent`'s `adopted`). A restart made over a standing refusal names
    /// aterm's at once: the relaunched agent's measures say nothing yet, and the person's
    /// `claude --resume` left standing would resume the conversation a second time.
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

    /// THE POINT MOVED UNDER A VERDICT THAT STOOD ([`AGENT_RECHECK`]): after
    /// an `await agent` step that ran out, whether the content moved since
    /// `seq` (one `await seq <seq> timeout 0`, the server's dirty check) and,
    /// where it did, whether the screen now is another point than `seen` —
    /// its review key another ([`Self::review_key`], what the look tells a
    /// new point by). A screen that only ticked (a footer, a clock: no
    /// letter of the point's moved) is waited on, `seq` moved to it so the
    /// next step asks from there; a new point ends the wait, and the look
    /// judges it. So a turn over before any sweep saw the worker busy is read
    /// within a step, and an idle screen costs one small request a step. A
    /// read since `seq` (the probe's, the host's) is asked first, with no
    /// request: another point there ends the wait, and the same one moves
    /// `seq` to it — what was read is not read again.
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

    /// The words an `await agent` on `seen` waits for (every verdict but the
    /// point's own), or `None` where that wait cannot stand for the content
    /// moving: a host without it; an act of the loop's own in flight (a
    /// `/model` answers idle to idle); a point neither idle nor a question; a
    /// wall (its own deadline wakes it), and a usage limit whose notice LEFT
    /// the screen while the policy waits on the reset it named
    /// ([`TurnEndState::waits_on_named_reset`]: Esc on the vendor's
    /// countdown) — its deadline is the reset, hours or days off, and the
    /// verdict is a sweep's, which a turn shorter than the sweep never moves
    /// (a person's turn the stub answered at once, measured on 2.1.299: no
    /// point read until the reset); a point a draft holds with nothing due
    /// ([`Self::draft_held`]), where the person's send or clear is the wake
    /// (measured on 2.1.289: a `MENU_DIVERTS` badge standing ten minutes after
    /// the draft was cleared, and after it was sent and answered at once);
    /// the point the last such wait woke from,
    /// read again unchanged (the verdict and the reading disagree); a point
    /// the loop's reader does not vouch for
    /// ([`aterm_phase::Reading::phase_authoritative`]), e.g. read before the
    /// agent drew itself, whose verdict is already `idle` and never moves; and
    /// a point of an agent still starting (no mode or busy frame read yet,
    /// except Codex, agy, Muse Code and Gemini CLI, whose footers name none).
    ///
    /// The mode is read off a frame the verdict does not move on: the bypass
    /// proof is the footer an earlier read showed ([`Self::footer`]), and the
    /// frame that first draws it is idle like the one before. Until a footer
    /// has been read, the content is waited on so that frame is read, or a box
    /// would be judged with no mode known.
    ///
    /// That window closes at the first busy frame ([`Self::busy_read`]) as well
    /// as the first footer: a busy footer names every mode but the default, so
    /// a session read busy with no mode runs the default mode (or is not Claude
    /// Code). Without that bound, a session that never shows a mode word would
    /// be read on every screen change for its whole life.
    ///
    /// `seen_key` is the review key of `seen`, built on first need and kept by the caller.
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

    /// The host's step at a break in the agent's own background work
    /// ([`IdleHost::at_background`]), on a busy read: offered once every read
    /// since `since` showed only that work
    /// ([`aterm_phase::ScreenReader::background_wait`]) for
    /// [`background_settle`], with no wall, limit episode, held stall, act the
    /// agent has not taken ([`TurnEndState::act_untaken`]), or credits hold
    /// ([`Guard::Credits`]; under `cfg`, the policy), and only while the
    /// host [`IdleHost::wants`] a
    /// point; the host's act is journaled (`HOST seq=<n> background <word>`).
    /// Any other read restarts the settle.
    ///
    /// The guard is whether the act was taken, not whether its point was judged: a break reads
    /// busy, so an agent that orchestrates all day never reaches a judged point. Only an
    /// untaken act holds the break back, so no host line is stacked on a continuation the agent
    /// has not read.
    ///
    /// A screen the session's transcript read as such a break
    /// ([`Self::transcript_wait`], BG-1: no row on the screen says how the
    /// turn ended) is one too.
    fn host_steps_in_background(
        &mut self,
        screen: &Screen,
        since: &mut Option<Instant>,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) {
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
        // The wall is read only for a read those two let through.
        let wall = if unfit.is_none() {
            reader.wall(&screen.rows)
        } else {
            None
        };
        // The usage limit's notice on the screen now, by the reset it names
        // ([`Self::limit_spent`]).
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
            // A busy read that is no break is the agent at work: the hold it
            // said is over. A wall over a break holds it.
            let hold = (guard == Guard::Wall).then_some(HoldSaid::Held(guard));
            self.say_hold(screen.seq, hold, review);
            return;
        }
        let at = *since.get_or_insert_with(|| self.fx.mono());
        let Some(host) = host.filter(|h| h.wants()) else {
            self.withheld = None;
            // As for a host that asks: the break's first moments, its
            // settle, are no hold; what was said stands, and nothing is said
            // yet.
            let waited = self.fx.since(at);
            let settled = waited >= self.setup.background_settle;
            if settled {
                self.say_hold(screen.seq, Some(HoldSaid::NoHostStep), review);
            }
            // The occupancy hold's clock, from this same read ([`occupancy_loop`]).
            self.hold_background(screen, at, waited, settled, cfg, review);
            return;
        };
        // One read of how long the break has stood, for the settle and the
        // occupancy hold's clock alike ([`occupancy_loop`]).
        let waited = self.fx.since(at);
        let settled = waited >= self.setup.background_settle;
        self.hold_background(screen, at, waited, settled, cfg, review);
        let held = if !settled {
            Some(Guard::Settle)
        } else if self.limit.is_some() && !self.limit_spent(Some(host.as_ref()), notice) {
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
            // The break's settle is its own first moments, not a hold: what
            // was said stands, and nothing is said yet.
            if guard != Guard::Settle {
                self.say_hold(screen.seq, Some(HoldSaid::Held(guard)), review);
            }
            return;
        }
        // Offered: the next guard, whatever it is, is said afresh, and so is
        // the next hold.
        self.withheld = None;
        self.hold_said = None;
        if let Some((line, kind)) = host.at_background() {
            review.note(&format!("HOST seq={} background {line}", screen.seq));
            // A turn the host typed: its answer is the harness's, never the
            // worker's work, and read as the turn it was
            // ([`IdleHost::at_background`]'s kind): the notice's answer is
            // never a done report; the release's, the worker's work resumed,
            // that says done is one, as at an idle point (before, every line
            // typed here was taken as the notice, and a long answer to a
            // release that said done was continued).
            self.host_turn(kind, screen.seq);
            self.hold_line(screen.seq, cfg, review);
            *since = None;
        }
    }

    /// The host's step at `point` ([`IdleHost::at_idle`]), taken when the host
    /// asks for one ([`IdleHost::wants`]) and the point is idle as vouched for
    /// by the reader of the program the server names now (one `status`: a
    /// generic read of a bare composer is no evidence), read with the
    /// terminal's cursor ([`aterm_phase::read_at`]: Claude Code's `idle` only
    /// at the prompt box holding it), with no box, wall, open limit episode or
    /// act of the loop's own in flight, nor usage credits the key keeps the
    /// session off ([`Guard::Credits`]) — nor a limit only the agent's
    /// transcript shows ([`Self::standing_holds`], asked last). `true` when
    /// taken (the loop then reads again). The loop asks nothing of the server
    /// while the step runs.
    ///
    /// A point a guard withheld where the agent RESTS — the point and its
    /// reading both idle or at a usage limit's notice, the reading one its
    /// reader vouches for — is the host's quiet point
    /// ([`IdleHost::at_withheld_point`]), where it settles what types
    /// nothing; what it settled is journaled `HOST seq=<n> <line>`. `asked`:
    /// whether the quiet point was asked already at this point or in this
    /// wait — set here once it is — so its first ask there is the point's
    /// own (`wake` false) and every later one a wake.
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
        // A carried wait the session's transcript says is over is the idle
        // point the loop read (BG-1 correction 3, [`Self::point_of`]), its
        // idle vouched for by the cursor as any idle point's is: a busy
        // reading was never asked whether the prompt box holds it.
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
        // The point's wall as its decision judged it (XT-2): a wall only the
        // transcript named withholds the step as the screen's own does, and a
        // screen wall the decision refused does not. Nothing is asked here.
        self.wall_as_decided(&mut reading, &point.screen);
        // A usage limit its grace past its reset is an idle point: the reset
        // of the notice on screen now, by the text the episode read its own
        // from ([`Self::limit_spent`]).
        let limit_wall = matches!(&reading.wall, Some(w) if w.kind.reads_limited());
        let notice = match (&point.phase, &reading.phase, &reading.wall) {
            (Phase::Limited { reset, .. }, _, _) | (_, Phase::Limited { reset, .. }, _) => {
                Some(reset.as_deref())
            }
            (_, _, Some(w)) if w.kind.reads_limited() => Some(w.reset.as_deref()),
            _ => None,
        };
        // …and so is the notice a session moved to another account left behind, repainted by
        // the conversation it resumed, until a turn has been asked under the new login
        // ([`TurnEndState::account_resume_owed`]): its reset is the old account's.
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
            // Offered: the next guard, whatever it is, is said afresh.
            None => self.withheld = None,
        }
        // THE QUIET POINT (F1-R4): withheld where the agent rests — idle, or
        // at a limit's notice, as a read its reader vouches for says (never
        // the start-up read, [`Guard::NotAuthoritative`]) — the host settles
        // what types nothing. Its retries are this loop's own points and
        // wakes: a guard may withhold every point for hours.
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
            // A task landed under Land gets no continuation from the host:
            // its relaunch's carry-on would start again the work that landed
            // ([`IdlePoint::done_under_land`]).
            let at = IdlePoint {
                done_under_land: self.autopilot(opts) == Autopilot::Land && self.turn_end.landed(),
            };
            let step = host.at_idle_on(at);
            if let Some(step) = &step {
                if !step.repeat {
                    review.note(&format!("HOST seq={} {}", point.screen.seq, step.line));
                }
                // A step that moved the session typed the harness's own keys
                // (the relaunch's line, its carry-on): the count says nothing
                // of a turn asked since ([`TurnEndState::host_moved`]).
                if step.moved {
                    self.turn_end.host_moved();
                }
                // A turn the host typed is awaited as the policy's own acts
                // are, and its answer is no work of the worker's.
                if let Some(kind) = step.typed {
                    self.host_turn(kind, point.screen.seq);
                }
                // A step that typed or moved the session is this idle point's
                // act: the occupancy hold's clock reads it as a decided point
                // that acted ([`occupancy_loop`]).
                if step.typed.is_some() || step.moved {
                    self.hold_point(&point.screen, true, Some(&opts.policy), review);
                }
            }
            // Passed over for the step: decided again once the host owns
            // nothing — unless the step moved the session, and the point
            // with it.
            let stood = !step.as_ref().is_some_and(|s| s.moved);
            self.host_held = stood;
            // A POINT THE STEP LEFT STANDING, the host still wanting one, is
            // not offered it again at once ([`Self::host_passed`]): only
            // after a step of the wait, or once the screen moves. Save where
            // the step took something while the host owns no turn end, at
            // most [`HOST_FOLLOW_UPS`] times a point ([`Self::host_follows`]):
            // the wait would decide that point next, so the host's follow-up
            // there (a give-up's release) goes first. A step that moved the
            // session left no point to stand, and is offered it again at
            // once: it spends none of the point's count, and keeps it
            // (F1-R9-L1) — only a turn the host typed (awaited, the point
            // over), a step of the wait or another point starts it again.
            // A MOVE THAT REPEATS A MOVE at the same point spends one, as a
            // step that moved nothing does: the point is
            // the one the last move was said at, so neither moved the
            // screen, and a word that only ever moves (`adopted` again) was
            // offered it again at once to the host's own cap.
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

    /// Whether the limit row the agent's conversation stands at, by its
    /// transcript ([`IdleHost::standing_limit`]), still holds the host's
    /// steps: until [`crate::harness::upgrade_drive::LIMIT_GRACE_S`] past the
    /// reset it names, and for good where it names none (a reset is never
    /// guessed; the policy's own probes, or a person, end it).
    fn standing_holds(&self, host: &dyn IdleHost) -> bool {
        let grace = crate::harness::upgrade_drive::LIMIT_GRACE_S;
        host.standing_limit().is_some_and(|limit| {
            limit.until.is_none_or(|reset| {
                i64::try_from(reset.saturating_add(grace)).is_ok_and(|due| self.now_unix() < due)
            })
        })
    }

    /// A usage limit left on screen past its reset is an idle point: `true` once the reset is
    /// [`crate::harness::upgrade_drive::LIMIT_GRACE_S`] behind now, by the episode's reset
    /// ([`Episode::reset_unix`]) or else the transcript's ([`IdleHost::limit_until`], asked only
    /// then). Claude Code leaves its notice up past the reset until something is typed, which
    /// would otherwise withhold every point for good. Past the grace neither [`Guard::Wall`] nor
    /// [`Guard::LimitEpisode`] withholds a point, so the host steps as at any idle point and the
    /// policy continues the agent by the wall's rule. Before the grace, or with no reset read,
    /// everything withholds: a reset is never guessed.
    ///
    /// `notice` is the usage-limit notice on screen now by its reset (`Some(None)`: naming
    /// none; `None`: no notice). The episode's reset counts only for the notice it was read from
    /// (same [`Episode::reset_text`]) or where no notice stands: an episode survives a retry,
    /// and a later limit that names no reset must not be judged spent by the older one.
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
    /// Say to `host` that the point it asked for was withheld by `guard`
    /// ([`IdleHost::withheld`]) — only while it [`IdleHost::wants`] one, and
    /// only when the guard is not the one last said ([`Self::withheld`]). A
    /// host that asks for no point is told nothing, and the next guard after
    /// it asks again is said afresh.
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

    /// THE HOLD JOURNALED ONCE PER CHANGE ([`HoldSaid`]): `hold` is said as
    /// `HOLD seq=<seq> <words>` (journal only, [`Review::note`]) when it is
    /// not the hold said last; `None` ends the hold said, so the next one is
    /// said afresh.
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

/// The box's first line as the badge and the ask carry it: the box's own first
/// row ([`prompt_box_first_row`], never a transcript row), line breaks
/// ([`limit::breaks_a_line`]) as spaces, trimmed, and cut to
/// [`APPROVAL_LINE_CELLS`] terminal cells, a wide character counting two
/// ([`aterm_grapheme::char_width`]).
fn approval_line(rows: &[String]) -> String {
    let first = prompt_box_first_row(rows).map_or("", |i| rows[i].as_str());
    cut_cells(first, APPROVAL_LINE_CELLS).0
}

/// `text` on one line — every character that breaks a line
/// ([`limit::breaks_a_line`]) a space — trimmed, and cut to `cap` terminal cells, a
/// wide character counting two ([`aterm_grapheme::char_width`]); `true` when
/// something was cut. The one sanitizer and cap the escalation's texts share
/// ([`approval_line`], and the subject and reason of `escalate::attention_text`).
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

/// The first `n` bytes of `s` on a character boundary.
fn fit_bytes(s: &str, n: usize) -> String {
    let mut end = s.len().min(n);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// The `attention=` a bare `meta` reply carries, decoded, or `None` when it
/// is unset (`-`), empty, or absent from the reply.
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

/// `digit` sitting alone in the composer with no box on the screen, as
/// `reader` reads them: the digit a press wrote after what it answered had
/// left on its own — the survey switch's `0` after the survey went (typed
/// text, not the placeholder the composer shows when empty).
fn stray_digit(reader: &dyn ScreenReader, screen: &Screen, digit: &str) -> bool {
    if reader.prompt(&screen.rows).is_some() {
        return false;
    }
    let composer = reader.composer(&screen.rows);
    typed_draft_in(composer.as_ref(), reader.placeholder(), screen)
        && composer.is_some_and(|(_, lines)| lines.len() == 1 && lines[0] == digit)
}

/// Text typed into the composer as `reader` reads it, not the placeholder it shows when empty:
/// a `0` pressed now could land in it. A draft can span rows ([`ScreenReader::composer`]): text
/// under the caret row is typed (the placeholder is one row), and so is text on the caret row
/// unless the cursor sits at column 2 of that row, where every program parks it on an empty
/// composer, and the program draws a placeholder ([`ScreenReader::placeholder`]; agy and Muse
/// Code draw none, so all their words are typed). A cursor on no row read is judged by its
/// column alone.
fn typed_draft(reader: &dyn ScreenReader, screen: &Screen) -> bool {
    typed_draft_in(
        reader.composer(&screen.rows).as_ref(),
        reader.placeholder(),
        screen,
    )
}

/// [`typed_draft`] of the composer as its reader already read it off `screen`
/// ([`ScreenReader::composer`]), `placeholder` its reader's [`ScreenReader::placeholder`].
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

/// Whether the composer's text begins with a `0` ([`ScreenReader::composer`]): the
/// survey's `0` heading what a person typed in the moment it was pending (`0hello`,
/// `00`).
fn heads_with_zero(composer: Option<&(usize, Vec<String>)>) -> bool {
    composer.is_some_and(|(_, lines)| lines.first().is_some_and(|l| l.starts_with('0')))
}

/// The line `watch` prints (and `supervise` says on stderr) when a rating card appears: `EVENT
/// survey seq=<n> dismiss with: aterm ctl @<sid> key 'if=^●.How.is.Claude.doing' 0`, the
/// guarded `0` that is the manager's one key to press there (no `@sid` named, none in the
/// command). The guard is the block's own ([`aterm_phase::SurveyBlock::guard`]), so it follows
/// the kind of card; any kind but the session survey names itself after the seq
/// (`kind=plugin`). The guard is quoted because `^` is a glob operator under zsh's
/// `extended_glob`. `EVENT`, so a monitor that wakes on EVENT lines wakes on it: a turn typed
/// while it is open whose first character is 1, 2 or 3 is taken as a rating.
///
/// A card the harness will not press ([`aterm_phase::SurveyBlock::withheld`]) carries no
/// command (`EVENT survey seq=<n> kind=unknown not pressed (<why>); read it with: aterm ctl
/// @<sid> text`): a guard it does not have would be improvised by whoever runs the line.
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

/// ` kind=<word>` for any card but the session survey, else nothing.
fn survey_kind_word(kind: aterm_phase::SurveyKind) -> String {
    if kind == aterm_phase::SurveyKind::Session {
        String::new()
    } else {
        format!(" kind={}", kind.name())
    }
}

/// `DISMISSED survey seq=<n>` — with ` kind=<word>` after the seq for any card but
/// the session survey.
fn survey_dismissed_line(seq: u64, kind: Option<aterm_phase::SurveyKind>) -> String {
    format!(
        "DISMISSED survey seq={seq}{}",
        kind.map(survey_kind_word).unwrap_or_default()
    )
}

/// What `phase` prints: [`render_phase`]'s lines, then `survey 0` when a rating card is open
/// ([`ScreenReader::survey_block`]), the key that dismisses it, or `survey withheld (<why>)` for
/// a card the loop would not press ([`aterm_phase::SurveyBlock::withheld`]), whose `0` is a
/// person's call. Last, `context <n>%` when the program's context reading is up
/// ([`ScreenReader::context`]: Claude Code's indicator, Codex's footer): that much is left
/// before the worker auto-compacts. Every line is `reader`'s, the worker's own program's.
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

/// The lines `aterm drive phase` prints, read by `reader` (the worker's own
/// program's; a caller with no session to ask takes [`aterm_phase::identify`]'s
/// for the screen): the phase word, then for a prompt its parsed kind,
/// command, description, one `note <text>` per note row (a Bash box's warning
/// rows — `Prompt::notes`), classification and options, and a question's
/// token for `aterm drive answer --box` ([`prompt_box_token`]); for busy,
/// `reason <zone>: <rule>` (which signal fired, and where — `whole screen, no
/// composer frame: output still changing` when a wait ran out on a screen
/// without the composer frame that kept changing, `no screen: no read
/// answered before the timeout` when the budget ran out in an outage before
/// any read did); for a limit notice, `message <text>` and `reset <text|->`.
pub fn render_phase(turn: &Turn, reader: &dyn ScreenReader, python_allow: &[String]) -> String {
    let rows = &turn.screen.rows;
    let mut out = format!("{}\n", turn.phase.name());
    match &turn.phase {
        Phase::Prompt => {
            if let Some((p, esc)) = phase_prompt(reader, rows) {
                out.push_str(&render_prompt(&p, esc, python_allow));
            }
            if let Some(token) = reader.prompt(rows).as_ref().and_then(prompt_box_token) {
                out.push_str(&format!("box {token}\n"));
            }
        }
        Phase::Busy => match busy_rule(reader, rows) {
            Some(b) => out.push_str(&format!("reason {b}\n")),
            // A real read always has rows: none is the timeout an outage
            // spent before any read answered (see `spent_turn`).
            None if turn.screen.rows.is_empty() => {
                out.push_str("reason no screen: no read answered before the timeout\n")
            }
            // Only a timed-out wait says this: output still arriving on a
            // screen without the composer frame (see `await_turn_from`).
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

/// The box `reader` reads on `rows`, in the fields `phase` prints, and the
/// line that says what Esc does on it ([`esc_line`] of the reader's
/// [`ScreenReader::esc`]): Claude Code's [`parse_prompt`] (a note per row;
/// Esc leaves every box, and the trust dialog's quits the program), any
/// other program's [`ScreenReader::prompt`] (Codex's box: its kind, command
/// or folder, its question, its numbered options).
fn phase_prompt(
    reader: &dyn ScreenReader,
    rows: &[String],
) -> Option<(Prompt, Option<&'static str>)> {
    match reader.program() {
        Program::Claude => parse_prompt(rows).map(|p| {
            // A box the whole read misses still leaves on Esc, as every
            // Claude Code box does.
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

/// What Esc does on a box, as `phase` prints it ([`ScreenReader::esc`]):
/// `cancel esc` where it leaves the box (a refusal, a step back); `exit esc`
/// where it QUITS THE PROGRAM — a trust dialog's: Claude Code's (`Esc to
/// cancel`, its cancel handler exits), Muse Code's, and Gemini CLI's and
/// agy's, which draw no Esc at all; `interrupt esc` where it ends the whole
/// turn and declines nothing — agy's and Muse Code's questions (`esc to
/// interrupt`). No line where nothing says what Esc does there.
fn esc_line(effect: Option<CancelEffect>) -> Option<&'static str> {
    match effect? {
        CancelEffect::Reject | CancelEffect::Back => Some("cancel esc"),
        CancelEffect::Exit => Some("exit esc"),
        CancelEffect::Interrupt => Some("interrupt esc"),
    }
}

/// Which busy rule fired on `rows`, read by `reader`, as `<where>: <rule>`:
/// Claude Code's [`busy_signal`] (its rules are the only ones a screen with
/// no reader's grammar is scanned by); another agent's own reader's: its
/// status row ([`ScreenReader::busy_guard`]), else the agent's own
/// background work its reader names ([`ScreenReader::background_wait`]:
/// agy's tasks, timers and subagents, Muse Code's backgrounded shells and
/// monitors), else its reading of the screen. A Codex's reader reads
/// nothing (aterm reads no Codex screen), so none.
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

/// The one line `watch` prints at a review point: `EVENT <phase> seq=<n>
/// <summary>`, the summary being, for a prompt, `kind=<k> classify=<read-only|
/// not-read-only:<reason>> command=<command>` (`classify=-` for a box that is
/// not Bash; a workflow's description stands in for its command); for a limit
/// notice, `message=<text> reset=<text|->`; otherwise the last row the worker
/// said. Each field is cut at 160 characters.
pub fn event_line(turn: &Turn, python_allow: &[String]) -> String {
    format!(
        "EVENT {} seq={} {}",
        turn.phase.name(),
        turn.screen.seq,
        event_summary(turn, python_allow)
    )
}

/// What a point's line says of it, read by the reader the screen names
/// ([`aterm_phase::identify`]; a caller of [`event_line`] has no session to ask): a box's kind,
/// verdict and command; else what [`said_summary`] says.
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
        let v = classify_command_with(&p.command, python_allow);
        if v.read_only {
            "read-only".to_string()
        } else {
            format!("not-read-only:{}", v.reason)
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

/// What a point that is no box `reader` reads says of itself: a wall's
/// message and reset; else the worker's LAST LINE — the last row it said,
/// with the rows above it that its line wrapped onto in the pane
/// ([`aterm_phase::line_start`], by the screen's width), joined with one
/// space: Claude Code's last row said as drawn (its `⏺` row, a notice), and
/// Codex's, agy's, Muse Code's and Gemini CLI's last words' last line
/// ([`said_line`]). Read a row at a time, a reply wrapped in a narrow pane
/// said only its tail: `EVENT idle seq=898 2)` in a 50-column Codex lab, and
/// `or 2)?` for agy's `… Which do you prefer (1 or 2)?` (the
/// `*-lab-reply-wrapped-50col` captures). A line past [`LINE_CHARS`] keeps
/// its END ([`clip_tail`]): what the worker ended on — the question, the row
/// the summary read alone before — never cut for the paragraph's opening. A
/// shell's last row (no reader's words) is that row alone; with no width
/// read, every last row is. A prompt whose box `reader` does not read is a
/// box of no kind.
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

/// The worker's LAST LINE as `reader` reads its words
/// ([`ScreenReader::said_tail`]: the rows of its reply, one line per row, its
/// marks and indent stripped): the last of them, and the ones before it that
/// are the rows above it its line wrapped onto in a pane `width` cells wide
/// ([`aterm_phase::line_start`]), joined with one space. With no width read,
/// or the last row not found on the screen, the last line alone.
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

/// THE SESSION'S SUBMISSIONS AT `screen` ([`Screen::submits`]) and the mark
/// of its conversation there: the letters of every row up to the last thing
/// said ([`last_said_index`], the rows [`point_key`] reads its last few of),
/// so a footer, a status row or a composer is none of it
/// ([`crate::supervise::policy::turn_end::screen_mark`]). `None` from a host
/// that counts none.
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

/// A TURN ASKED SINCE A POINT WHOSE KEY CANNOT SEE IT: the session's count
/// at `now` past the one the point was read at (`then`, [`submit_mark`]),
/// and its conversation drawn since — a prompt answered at once with the
/// very rows the point's key reads (the same words typed again and answered
/// the same, the turn before scrolled above them). The look judges it as a
/// new point; the policy folds it in once
/// ([`crate::supervise::policy::turn_end::TurnEndReading::submits`]). A
/// count that moved over the same conversation is a submission not drawn
/// yet; no count is no evidence.
fn submitted_over(then: Option<(u64, u64)>, now: &Screen) -> bool {
    matches!((then, submit_mark(now)), (Some((n0, m0)), Some((n, m))) if n > n0 && m != m0)
}

/// What makes two review points the same one (never the seq, which any footer tick moves),
/// read by `reader`, the loop's one grammar ([`Session::review_key`] passes
/// [`Session::reader`]).
///
/// A box `reader` reads is the request it makes ([`box_key`]). Any other point, and a prompt
/// whose box `reader` does not read, is the phase, what it says ([`said_summary`]), the status
/// row, and the words of the last [`KEY_ROWS`] non-blank rows of the transcript up to the last
/// thing said (or, for that prompt, up to its box, [`prompt_box_span`], whose rows count
/// verbatim). Anything the worker, a tool or the manager adds lands in those rows, while a
/// footer or a banner does not; letters only, so a ticking timer or a blinking glyph does not
/// make a row new.
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

/// The words of the last [`KEY_ROWS`] non-blank rows of `above`, letters
/// only, one row per line, appended to `key`.
fn push_words_above(key: &mut String, above: &[String]) {
    let said: Vec<&String> = above.iter().filter(|r| !r.trim().is_empty()).collect();
    for r in &said[said.len().saturating_sub(KEY_ROWS)..] {
        key.extend(r.chars().filter(|c| c.is_alphabetic()));
        key.push('\n');
    }
}

/// The review key of the box `p`, read on `rows`: what it asks, never how it is laid out, so a
/// redraw of the same box is not read as the box answered and a new one raised. Four lines:
///
/// 1. `prompt <kind> <whole|cut>`, and ` form=<single|multi|preview|review|unknown>` for a
///    question dialog: whether the reader read the box whole (its title on the screen).
/// 2. Its options' numbers, in order (`-` for one drawn with none).
/// 3. Its body's digest: every row [`approval_loop::box_rows`] keeps (the title with its origin
///    and `(unsandboxed)`, so a Write and an Edit on one path differ) reduced to its content
///    characters ([`is_layout`] drops whitespace and box drawing), so a re-wrap reads the same
///    while another command, diff, note or label is another digest.
/// 4. For a box read whole, the last [`ABOVE_LETTERS`] letters of the transcript above the
///    pending call's echo ([`before_the_echo`]): output the worker appended since (the same
///    `git push` asked again after it failed) is a new point; a re-wrap, the tool row's blinking
///    `⏺` or rows scrolling in at the top are not. For a box not read whole, the words of the
///    last [`KEY_ROWS`] non-blank rows above its span.
///
/// Not in it: the auto-deny countdown, the focus marker `❯`, blank rows, and Codex's `›` on its
/// focused option. Checkbox marks, typed free text and a focus-following preview stay in, so
/// nothing that looks at a box while it waits compares this key ([`file_request_key`]).
///
/// Known residuals: two boxes differing only in whitespace (`rm -rf /tmp/x /` and
/// `rm -rf /tmp/x/`), back to back, are one point; a pane too short to hold [`ABOVE_LETTERS`]
/// letters above the echo reads a row scrolling in as a new point. The key only ever passes
/// over a point already handed over; it never causes a press.
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

/// The transcript above a box (`above`: every row over its title) less the pending call's
/// echo: the vendor's own row group right over the box, which repeats what the box asks
/// (`Running 2 shell commands…` over `⎿  $ <command>`, `⏺ Update(notes.txt)`, Codex's
/// `• Running <command>`), with the rule and blank rows between it and the box ([`box_key`]'s
/// line 4). The group is the run of rows with content right over those, blank-separated from
/// what came before in every layout measured; a group with no gap row above it is not known
/// to be an echo and stays. Left in, a long command's echo would fill [`ABOVE_LETTERS`], so
/// the same command asked again after it failed would read as the box asked before.
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

/// A character that is layout, not content ([`box_key`]): whitespace (a wrap point, an indent,
/// a gutter's padding, a no-break space) and the box-drawing block (U+2500–U+257F): the `│`
/// gutter bars, the `─` rules and the `╌` diff dividers, whose length follows the pane's width.
fn is_layout(c: char) -> bool {
    c.is_whitespace() || ('\u{2500}'..='\u{257F}').contains(&c)
}

/// What a file box asks (an Edit, notebook and IDE diff included, a Write, an Overwrite or a
/// Read box `reader` reads whole), for the one read after a step its first row stood
/// ([`Session::wait_for_next`]): its kind, then [`box_key`]'s digest of its rows above its
/// first option. `None` for any other point.
///
/// These kinds' first body row, the path the wait is anchored on ([`box_anchor`]), is one two
/// requests share (two Edits on one file with different diffs), so only a read sets them
/// apart; a command, a url or a question opens with what it asks. Left out, as moving while
/// the same request stands: the options, the footer, the auto-deny countdown and the letters
/// above the box, so someone part-way through answering it ends no wait.
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

/// The first [`LINE_CHARS`] characters of `s`, on one line.
fn clip(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(LINE_CHARS)
        .collect()
}

/// The last [`LINE_CHARS`] characters of `s`, on one line: `…` and the rest
/// where it is longer, so a line keeps what it ended on.
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

/// `watch`'s `EXIT` reason, on one line: the failed request, named as the
/// session going when that is what the host said — `ERR exited` to a wait in
/// progress when the session ends, `ERR no such session` to a request after.
pub fn exit_reason(err: &str) -> String {
    let err = one_line(err);
    if err.contains("ERR exited") || err.contains("no such session") {
        format!("session gone ({err})")
    } else {
        err
    }
}

/// `s` on one line: a control character (a newline in a client's error)
/// becomes a space.
fn one_line(s: &str) -> String {
    s.trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// An informational line on a diagnostic stream (the engine tests'
/// `supervise` and `await_turn`): flushed, and a failed write is not the
/// loop's end.
#[cfg(test)]
fn log_line(log: &mut dyn Write, line: &str) {
    let _ = emit(log, line);
}

/// Write one line and flush it: a reader of the loop's lines wakes per line.
fn emit(out: &mut dyn Write, line: &str) -> Result<(), String> {
    writeln!(out, "{line}")
        .and_then(|()| out.flush())
        .map_err(|e| format!("cannot write a line to stdout: {e}"))
}

fn render_prompt(p: &Prompt, esc: Option<&str>, python_allow: &[String]) -> String {
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
        let Verdict { read_only, reason } = classify_command_with(&p.command, python_allow);
        if read_only {
            out.push_str("classify read-only\n");
        } else {
            out.push_str(&format!("classify not-read-only {reason}\n"));
        }
    }
    for (n, text) in &p.options {
        out.push_str(&format!("option {n} {text}\n"));
    }
    // What Esc does ([`phase_prompt`]). On Claude Code it leaves every box: a
    // footed one names it in its `Esc to …` footer, a footerless one (Fetch,
    // the network request, Chrome, the plan, held-message and goal dialogs, a
    // setup dialog) marks its refusal ` (esc)` or answers Esc with its
    // select's cancel — and the trust dialog's quits the program.
    if let Some(esc) = esc {
        out.push_str(esc);
        out.push('\n');
    }
    out
}

/// The compact result the engine tests' `supervise` ([`Session::supervise`])
/// answers: the phase lines (read by `reader`, [`render_phase`]), then the
/// prompt box verbatim, or the last 28 non-blank rows.
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

/// The `--` line and the rows under it ([`render_result`]).
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

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time — `aterm_types::rfc3339`'s
/// formatter, the workspace's one copy of the calendar.
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
/// The decision on a box read whole from the history, as the loop makes it
/// (`tape::explain` decides a recorded one again so).
pub(crate) use approval_loop::whole_box_decision;
#[path = "box_head.rs"]
mod box_head;
/// The join of a box's head read from the history, and the readers of the
/// replies it is made from, which a tape's reader rebuilds it with
/// (`tape::explain`).
pub(crate) use box_head::{SEARCH as HEAD_SEARCH, head_row, head_rows, joined_box};
#[path = "claim.rs"]
mod claim;
/// The requests a watching-only loop may not send, which a harness tape's
/// control door counts among its acts (`tape::ctl::is_act`).
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
/// A ledger's wall's words read back ([`approvals::UsageAttempt::met_ms`]).
pub(super) use wall_confirm::words_kind;
#[path = "wall_memory.rs"]
mod wall_memory;
pub use turn_end_loop::standing_rules;
/// The composer's cursor-row guard, for the live upgrade's pressed notice too
/// (`harness::upgrade_drive`).
pub(crate) use turn_end_loop::{composer_guard, composer_guard_at};
/// The act executor's fold of an act's text and the loop's reader of the
/// standing rules, for the policy's tests of what agy reads back
/// (`policy/turn_end_agy_message_tests.rs`).
#[cfg(test)]
pub(crate) use turn_end_loop::{one_turn, standing_rules_in};

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "run_frame_tests.rs"]
mod frame_tests;
