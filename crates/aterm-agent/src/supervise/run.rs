// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor loop over the control verbs: wait for the worker's turn to
//! end ([`Session::await_turn`]), then either answer an approval box the
//! approval policy answers itself or hand the screen to the manager — once, and
//! exit ([`Session::supervise`]), or as one line per decision while it keeps
//! watching (`Session::watch`). Every request goes through one [`Ctl`] seam
//! (`aterm-ctl` in production, a scripted mock in tests); server features newer
//! builds add (`text … tail=`, `await gone`, `key if=`) are probed once and
//! remembered, so the loop runs against an older host too. A request the server
//! did not serve (the hosting instance went away, e.g. a self-update handing
//! sessions to a new instance under the same `@sid`, or the server turned the
//! connection away) does not end the loop: the outage is ridden out
//! ([`Session::ride_out`]) and the loop looks again from a fresh read.
//!
//! With `--mail` ([`SuperviseOpts::mail`]) a second [`Ctl`], the mail lane
//! ([`super::mail::Lane`]), parks `await inbox` on the manager's own session
//! from its own thread, prints `MAIL …` per delivery through the loop's sink,
//! and hands the worker's `report`s to the loop, which folds each into the idle
//! point of the same turn (`Session::hold_for_report`). The worker's socket
//! sees no extra request, and without the flag every line is byte-identical.
//!
//! A usage limit is a decision point `watch` handles alone: one weekly limit
//! can stop the manager and the worker at once, leaving nobody to act. On
//! `EVENT limited` the watcher never exits on a limit — a budget that would
//! run out before a reset the notice named is stretched past it
//! ([`Session::extend`]) — and the turn-end policy types the continuation
//! 150 s past the reset, a session's share of a minute later (the rules
//! file's text with it), as at any other turn end it acts on (below); a
//! limit the policy does not wait out (`resume_limits = false`)
//! sets the worker's `attention` meta and, while the fabric is connected,
//! posts `kind=control` mail to the manager ([`Session::escalate`], once an
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
//! ([`Retry`], [`Session::open_episode`]) — the attention set again, the
//! manager mailed once, not once a retry. So does the notice still up after
//! an Autopilot Off park, which took the episode's badge down
//! ([`Session::lower_badges`]).
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
//! escalated as `<program> <kind>: <subject> (<reason>)`, mailed only while the
//! fabric is connected (`escalate.rs`). [`Session::run_hosted`] is the in-GUI
//! host's entry: the same loop under [`SuperviseOpts::hosted`], ended by a stop
//! flag.
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
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::approvals::{self, Ledger};
use super::classify::{DEFAULT_PYTHON_ALLOW, Verdict, classify_command_with};
use super::config::Approve;
use super::config::{Autopilot, SupervisorConfig};
use super::journal::{CLEARED_GOAL, CLEARED_MEMORY, CLEARED_TURN_END, Journal};
use super::limit::{self, ResetSpec};
use super::mail::{self, Delivery, MailOpts};
use super::phase::{Phase, busy_signal, last_said_index, last_said_row, status_row};
use super::policy::approval::{Decision, footer_mode};
use super::policy::exception::Exception;
use super::policy::reset::{
    KeepWhy, ResetAsk, ResetEnd, ResetGo, ResetNo, ResetOutcome, ResetWord,
};
use super::policy::turn_end::{
    AccountAdvice, AccountBad, AccountPoint, GOAL_NOTE_HEAD, ModelSwitch, ProbeGate, Reach,
    TurnEndAction, TurnEndReading, TurnEndState, TurnEndTiming,
};
use super::prompt::{
    CancelEffect, Prompt, PromptKind, PromptV2, parse_prompt, prompt_box_first_row, prompt_box_span,
};
use super::report::ReportOpts;
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
    /// An input verb that wrote behind input the worker has not read yet: its
    /// `OK` carries `warn=input-unread input_bytes=<n> input_wait_ms=<ms>`.
    ///
    /// The warning ends the reply's first line. For `key`/`send`/`paste` that
    /// is stdout; for `turn` it is the verdict, which both transports print on
    /// stderr while stdout holds the settled rows, so a turn's warning is read
    /// off [`Self::turn_verdict`], never off its rows.
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
    /// 2026-10-16: every nudge `drive task` typed landed so).
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
    /// The request was not served: the client could not reach the instance
    /// hosting the session, or lost it mid-exchange — the connection closed
    /// before a reply (`server closed the connection without responding`,
    /// measured when aterm 0.82.0 handed its sessions to 0.83.0 under a running
    /// `watch`), a reply cut short, a socket nothing listens on or that is gone,
    /// a reset, a broken pipe, a socket timeout — or the server turned the
    /// request away before it did anything with it (`TURNED_AWAY`: its
    /// admission queue full, the token sent another instance's, or a main
    /// thread stalled past the bar). Any other `ERR …` line is the server's
    /// answer and never this: `ERR no such session` and `ERR exited` say the
    /// session is gone (though while an outage is being ridden out a client
    /// takes `no such session` for one more request not served —
    /// [`Self::unserved`]).
    ///
    /// AND EXIT 75 ([`aterm_ctl::EXIT_REPLACED`], the client's own constant,
    /// never a copy of its number), whatever its note says: `aterm-ctl` exits
    /// so on a request an update's Commit cut and does not ask again —
    /// an `await seq <n>` or `await inbox since=` whose anchor names the old
    /// process, an armed wait, a `turn`, a `post` — or that no successor
    /// answered at all. Read as an answer, `aterm drive await-turn
    /// --reconnect-s` and the `--mail` lane would die at an update instead of
    /// riding it out. Nothing else exits 75, and a request that may have
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

    /// A request not served ([`Self::lost`]) that the server certainly did
    /// nothing with: turned away unread (`TURNED_AWAY`), or never sent —
    /// the connection refused, its socket gone, no instance taking a request
    /// on it (`UNSENT`). Every other request not served was cut
    /// mid-exchange — `aterm-ctl`'s exit 75, a connection closed, reset or
    /// timed out under it (`CUT`) — and may have been carried out: a `post`
    /// may have landed, a `turn` been typed. Only this one may be said not
    /// to have happened.
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
    /// client that rides an outage out: the loop (`Session::unserved`),
    /// the `--mail` lane and `aterm drive task --wait`.
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
/// for both, which stays here.
const CUT: &[&str] = &[
    "server closed the connection without responding",
    "server hung up before the complete response",
    "Connection reset",
    "forcibly closed",
    "Broken pipe",
    "Resource temporarily unavailable",
    "timed out",
];
/// What the client prints when a request was never sent ([`CtlReply::lost`]
/// and [`CtlReply::untouched`]): the OS's refusal of the connect (the Unix and
/// the Windows spellings), and `aterm-ctl`'s connect phase's `no answer from
/// <sock> within <N>s: the instance took no request, so none was sent — …`
/// (exit 2) for a connection no instance took a request on.
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
/// The server's `ERR` lines that turn a request away BEFORE anything is done
/// with it, so nothing was served ([`CtlReply::lost`]), each a whole leading
/// phrase: `ERR control server busy; retry` (the listener's admission queue is
/// full — an explicit retry signal), `ERR auth` (the token sent is not the
/// one this instance holds: the `latest` alias moved between the client's
/// connect and its token read, or the instance restarted — the client re-reads
/// the token on its next run), and `ERR main thread stalled <N>s;
/// retry` (a verb that needs the window's main thread, refused before it was
/// posted because the main thread has had no heartbeat for the stall bar while
/// it had work — stuck in AppKit, say — so the verb does not wait out its whole
/// reply deadline). The last may also follow a verb's own prefix
/// ([`turned_away`]).
const TURNED_AWAY: &[&str] = &[
    "ERR control server busy",
    "ERR auth",
    "ERR main thread stalled",
];

/// The stalled main thread's refusal as it reads after a verb's own
/// `ERR <what failed>: ` prefix.
const MAIN_THREAD_STALLED: &str = "main thread stalled";

/// Whether an `ERR` line turned its request away unserved: a [`TURNED_AWAY`]
/// phrase leads it, or the stalled main thread's refusal follows a verb's own
/// prefix (`ERR input dispatch failed: main thread stalled 7s; retry`
/// — `key`, `send`, `tab`, `find` and their like say what failed first). The
/// refusal is given before the verb is posted, so nothing was done either way.
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

    /// A handle that cuts the request in flight short — and every one after
    /// it — from another thread: what ends the mail lane's parked `await
    /// inbox` (20 s a step) the moment the loop has its result, so
    /// `supervise --mail` hands its point back at once and `watch --mail`'s
    /// process ends with its last line ([`Session::run_loop`]). `None` for a
    /// client that cannot: the loop then ends within one step.
    fn interrupter(&self) -> Option<Interrupter> {
        None
    }
}

/// [`Ctl::interrupter`]'s handle.
pub type Interrupter = Box<dyn Fn() + Send + Sync>;

impl Ctl for crate::CtlClient {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.run_raw(args)
    }
    fn interrupter(&self) -> Option<Interrupter> {
        self.cutter()
    }
}

/// Server features probed once per process (`None` = not yet asked).
#[derive(Debug, Default, Clone, Copy)]
pub struct Caps {
    /// `text --json tail=<n>` (a newer build sends only the last n rows).
    pub tail: Option<bool>,
    /// `await gone <re>`.
    pub gone: Option<bool>,
    /// `key if=<re> <key>` — the press happens only if a visible row matches.
    pub key_if: Option<bool>,
    /// `key if-gen=<epoch>.<seq> …` — the press happens only if the screen
    /// generation is still the judged read's (probed from `help key`, before
    /// the first approval).
    pub gen_fence: Option<bool>,
    /// `turn submit=guarded:<re>` — Enter only while the cursor's row still
    /// matches (learned from the first turn the turn-end policy types).
    pub guarded_submit: Option<bool>,
    /// `send if-gen=<epoch>.<seq> …` — the write happens only if the screen
    /// generation is still the judged read's (probed from `help send`
    /// before the turn-end policy's first fenced write).
    pub send_gen: Option<bool>,
    /// `await agent <word>[,<word>…]` — the server's own agent verdict, which
    /// it pushes as it moves. Learned from the first screen read:
    /// the server whose `text --json` carries the screen generation (`gen`)
    /// is the one that publishes the verdict, so it costs no request of its
    /// own; a refusal of the wait unlearns it.
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
    /// The step moved the session: it typed into the agent (a notice, a
    /// carry-on) or ended it, so the point is gone and the turn it started (or
    /// the new agent's first point) is the loop's next. The screen may still
    /// show the old point for a moment; it is not decided again. `false`: it
    /// typed and ended nothing (a wait, a last word), and the point stands, the
    /// loop's to decide once the host owns nothing.
    pub moved: bool,
    /// The step typed a turn into the agent (the upgrade's notice, a carry-on)
    /// whose answer is the loop's next point: awaited like the policy's own
    /// acts and counted as no worker work for the continue back-off
    /// ([`super::policy::turn_end::TurnEndState::host_typed`]). `false` for a
    /// step that ended the agent and typed nothing.
    pub typed: bool,
    /// The step repeats the host's last wait at this point (a person's hold,
    /// looked at every twenty seconds, `upgrade_drive::after_attended`): the
    /// journal keeps its first look and the step that ends it, not the looks
    /// between, so they do not push useful rows out of the kept journal.
    pub repeat: bool,
}

/// Which of the loop's guards withheld a point from a host that asked for one
/// ([`IdleHost::withheld`]). The first seven are the checks of a break in the
/// agent's own background work ([`Session::host_steps_in_background`]), in
/// order; `NotIdle`, `NotAuthoritative`, `Prompt`, `ActInFlight` and again
/// `Wall` and `LimitEpisode` are those of an idle point
/// ([`Session::host_steps_here`]); `SwitchOpen` and `WakeSettle` are both's,
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
    /// Codex's save-then-wait switch is open: it owns the session until it
    /// closes, and no upgrade notice is typed into its wind-down or hold
    /// ([`crate::supervise::policy::turn_end::TurnEndState::switch_open`]).
    SwitchOpen,
    /// The Mac woke less than its settle ago (30 s and the session's share
    /// of 60 s, [`Session::settling`]): the network may not be back, and a
    /// turn the host types now — the upgrade's notice, a carry-on — would
    /// meet an API it cannot reach. Offered once the settle has passed.
    WakeSettle,
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
        Guard::SwitchOpen,
        Guard::WakeSettle,
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
            Guard::SwitchOpen => "switch-open",
            Guard::WakeSettle => "wake-settle",
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

/// The loop's next try ([`IdleHost::waiting`]): when its wait ends, as a unix
/// second, and the policy's reason (`api-error retry 1`). Only an API wall's
/// retry wait is a next try ([`TurnEndAction::WaitUntil`]'s `retry`); other
/// waits are not told to the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitPlan {
    /// Unix seconds at which the loop decides the point again.
    pub at_unix: i64,
    /// The policy's words for the wait.
    pub why: String,
}

/// A wait as it was last told to the host ([`Session::tell_wait`]): the
/// deadline on the loop's own clock, and the unix second it was told as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ToldWait {
    at: Instant,
    unix: i64,
}

/// When the turn-end policy decides again at the point still showing, and
/// why ([`Session::turn_end_due`], set by [`Session::set_due`]): the deadline
/// on BOTH clocks — the `Instant` it was set as, and the unix millisecond that
/// `Instant` named on the wall clock when it was set. It falls at the first
/// to come ([`Session::due_at`]). Awake, they agree. A Mac's sleep stops the
/// `Instant` (`CLOCK_UPTIME_RAW` does not count it) and never the wall
/// clock: a wait set for a weekly reset ends at the reset however long the
/// Mac slept. The audit of 2026-10-09 found three tabs left 11.6-14.1 h past
/// a weekly reset their `Instant` waits had slept through. A wall clock
/// stepped back delays the wall time and never the `Instant`. (A due the
/// sleep carried past is moved to the wake's settle once the session's watch
/// has seen the sleep, [`Session::woke`]; the wake's settle is a `Due` too.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Due {
    /// The deadline as the `Instant` it was set as.
    pub(super) at: Instant,
    /// The same deadline on the wall clock, unix milliseconds, as it read
    /// when the deadline was set ([`Session::unix_ms_of`]).
    pub(super) unix_ms: i64,
    /// Why: the policy's words.
    pub(super) why: String,
    /// A wall's next try ([`TurnEndAction::WaitUntil`]'s `retry`), the only
    /// wait told to the host ([`Session::tell_wait`]).
    pub(super) retry: bool,
}

/// The least time between two of the accounts' news that end a LATCHED
/// session's wait ([`Session::account_edge`]): one more ask of a host that
/// refused its relaunch, a minute at the soonest.
const ACCOUNT_NEWS_EVERY: Duration = Duration::from_secs(60);

/// How far the wall second of a wait's deadline ([`Due::unix_ms`]) may move
/// from the second it was told as before it is told again
/// ([`Session::tell_wait`]). The same `Instant` set again after a sleep
/// names a later wall second (a sleep stops an `Instant`), and that is
/// news; the same `Instant` set again awake moves
/// it by the milliseconds between the two clocks' reads, which rounding can
/// turn into a second, and that is none.
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

/// A host's part at the loop's idle points ([`SuperviseOpts::idle_host`]): the
/// window's host takes the live upgrade's step, and a relaunched agent's owed
/// continuation, at an authoritative idle point inside the loop, which keeps
/// its back-off, typing budget, wall tracks, claim and badges across it.
pub trait IdleHost: Send + Sync + std::fmt::Debug {
    /// The host asks for the next idle point: read at each point the loop
    /// reaches and at each wake of its wait ([`WAIT_STEP`], the settle the
    /// upgrade's own gate asks for anyway) — nothing polls for it.
    fn wants(&self) -> bool;
    /// When the host will next want a point, if it already knows: a look it
    /// scheduled (the upgrade's short ladder after an unread look, 5 s then
    /// 15 s). The loop's wait ends there, so [`Self::wants`] is read at that
    /// instant and not at its next [`WAIT_STEP`] wake (else unread looks 5 s
    /// apart are taken 21 s apart). The default: never known ahead.
    fn wants_at(&self) -> Option<Instant> {
        None
    }
    /// Take the host's step. The loop is at an authoritative idle point
    /// with no box, no wall, no limit episode and no act of its own in
    /// flight, and has typed nothing there — the host's step goes FIRST,
    /// before the turn-end policy acts on the point. The loop reads the
    /// screen afresh after it, and journals the step's word when there was
    /// one (`HOST <what> step=<word>`), so the session's journal carries the
    /// upgrade's and the relaunch's acts beside its own. Whether the step
    /// MOVED the session ([`HostStep::moved`]) says what becomes of the
    /// point: one it left standing is decided again once the host owns
    /// nothing ([`Session::host_held`]); one it moved is gone.
    fn at_idle(&self) -> Option<HostStep>;
    /// The host owns the session's turn ends for now — the live upgrade's
    /// wind-down, from the state of the upgrade itself, never from the
    /// screen: the turn-end policy types nothing ([`TurnEndReading::upgrading`]).
    fn owns_turn_end(&self) -> bool;
    /// A natural break in the agent's own background work: its turn is over,
    /// the composer drawn, and only work it started runs (a workflow, a
    /// background agent, a shell, a Codex background terminal;
    /// [`aterm_phase::ScreenReader::background_wait`]), the break has stood
    /// [`background_settle`], and there is no wall, limit episode, stall or
    /// untaken act (`TurnEndState::act_untaken`). Offered only while the host
    /// [`Self::wants`] a point. The host may type one line that ends nothing:
    /// the upgrade's notice (first, or a re-ask after `upgrade::REASK_S`) or
    /// the release line owed by a give-up, void or stop; or it may give up
    /// asking. It returns what it took (`Some`, journaled `HOST seq=<n>
    /// background <word>`) or `None`. Ending the agent is an idle point's alone.
    fn at_background(&self) -> Option<String> {
        None
    }
    /// End the agent and relaunch it on its own conversation, here, for `why`
    /// ([`super::policy::turn_end::TurnEndAction::Restart`]). Returns the step's
    /// word ([`crate::harness::relaunch::restart_here`]: `adopted`, `continued`
    /// when the host also typed its carry-on, `wait:…`, `refused:…`/`failed:…`),
    /// or `None` where this host makes no restart and the point is the
    /// person's. The default makes none.
    fn restart(&self, why: &super::policy::turn_end::Restart) -> Option<String> {
        let _ = why;
        None
    }
    /// Whether [`Self::restart`] can be asked here at all — the agent one
    /// this host relaunches, under `[harness] relaunch` — so the turn-end
    /// policy asks for no restart that cannot be made
    /// ([`super::policy::turn_end::TurnEndReading::restartable`]). The
    /// default: none.
    fn can_restart(&self) -> bool {
        false
    }
    /// Whether the explicit `restart` would be CARRIED for the agent now: the
    /// host's one predicate for every word of a stall (aterm's
    /// `restart_act::offered`: a snapshot whose relaunch brings back what the
    /// act ends, [`crate::harness::relaunch::carries_restart`], the agent in
    /// front, and the host's hand the one on the session). `[harness]
    /// relaunch = false` does not limit it: it does not refuse a person's
    /// explicit restart (ruling 1, 2026-10-08). `Some(promised)` where it
    /// would: the frozen mail and the warning name that restart instead of
    /// `signal term` and [`Self::resume_command`] — as the server's own words
    /// do — and, only where `promised` (its exit relaunched on the agent's
    /// own conversation, [`crate::harness::relaunch::resumes_on_exit`]: never
    /// a Codex's), say "aterm relaunches it on its conversation". The
    /// default: `None`.
    fn restart_offered(&self) -> Option<bool> {
        None
    }
    /// The loop holds for a stall the server published (`true`,
    /// [`Session::hold_for_stall`]), or one it held lifted (`false`,
    /// [`Session::thaw`]). A NOTICE of what this loop saw, as it saw it — it
    /// lags the stall (a thaw is seen at the loop's next look) and a run that
    /// ends holding one never says it lifted — so it says nothing of how the
    /// agent ended: whether an exit was the stall's remedy's (U1) is the
    /// explicit restart's stamp, on disk before its signal, which aterm's
    /// host looks up instead. The default ignores it.
    fn stalled(&self, held: bool) {
        let _ = held;
    }
    /// Whether an agent the stall's remedy ends here is RELAUNCHED on its
    /// conversation by this host (U1: its relaunch on exit, under `[harness]
    /// relaunch`) — so the loop may take the remedy itself once the stall
    /// has stood past its bound ([`Session::stall_remedy`]). The default:
    /// no, and the remedy stays a person's.
    fn relaunches_after_stall(&self) -> bool {
        false
    }
    /// The agent's process as the host last read it — the tab's foreground
    /// group leader — which the stall's remedy names (`restart pid=<n>
    /// if-stalled`, refused unless that very process still leads the tab).
    /// The default: none, and the remedy names the tab's group.
    fn agent_pid(&self) -> Option<u32> {
        None
    }
    /// Nobody has asked the session anything, by the host's evidence: the
    /// conversation's record holds no prompt of a person's or orchestrator's
    /// (`crate::harness::upgrade::TaskScan`). Read at every turn end beside
    /// the screen's launch card ([`TurnEndReading::taskless`]); the turn-end
    /// policy types nothing into such a session. The default leaves it to the
    /// screen.
    fn taskless(&self) -> bool {
        false
    }
    /// THE HOST STARTED THIS LOOP AS THE SESSION'S AUTOPILOT LEFT OFF: it
    /// stopped the session's last worker under Off, and the person drove the
    /// session since. The acts a previous loop typed, seeded from the
    /// ledger, still count toward the budget but carry no task — save for a
    /// turn already running as the loop attaches
    /// (`TurnEndState::start_lifted_off`). The first point it finds idle is
    /// backed off and acted on as any loop's first point is, never left to
    /// anyone: no read saw that turn run. Read once, at the loop's start.
    /// The default: no — a loop started for any other reason (a new agent, a
    /// changed policy, a restart) carries what its ledger shows.
    fn lifted_off(&self) -> bool {
        false
    }
    /// A read since the last point saw the session busy: a turn ran (the
    /// agent's, a person's, an orchestrator's, or one the host typed). What the
    /// host counted of the points before starts over. The default ignores it.
    fn turn_ran(&self) {}
    /// A DECIDED IDLE TURN END of a Claude Code session (a decision's read,
    /// not the books' reread after an act): where the host reads the usage tap's
    /// file of the session's own process, records a fresh reading for its
    /// login and judges whether a tapped process's tap went silent
    /// (`crate::harness::tap`). It types nothing and decides nothing: a
    /// reading moves no session. The default reads nothing.
    fn usage_tap(&self) {}
    /// Information for a person from the session's supervisor, nothing asked
    /// and no badge: the host records it where a person looks (the window's
    /// Messages). Its first line is the state (a short title); further lines
    /// are detail. The default records nothing.
    fn inform(&self, text: &str) {
        let _ = text;
    }
    /// A limit episode opened (`true`, [`Session::open_episode`]) or closed
    /// (`false`). While one stands the loop offers the host no point, so the
    /// host holds its own clocks (the upgrade's re-ask and READY clocks), and
    /// the host is not read as owning turn ends
    /// ([`super::policy::turn_end::TurnEndReading::upgrading`]): the wall's
    /// own rule waits out the limit. The default ignores it.
    fn limited(&self, open: bool) {
        let _ = open;
    }
    /// The reset the agent's standing usage limit names in its transcript,
    /// unix seconds, past or to come
    /// (`crate::harness::upgrade::transcript_limit_reset`), or `None`. Asked
    /// where a limit's notice or episode would withhold a point and names no
    /// reset of its own ([`Session::limit_spent`]: a notice still shown
    /// [`crate::harness::upgrade_drive::LIMIT_GRACE_S`] past its reset is an
    /// idle point), and where a notice names a clock time but no day
    /// ([`Session::transcript_day`]). The default reads none.
    fn limit_until(&self) -> Option<u64> {
        None
    }
    /// THE AGENT'S OWN TRANSCRIPT, its tail: the last 256 KiB of the
    /// conversation the Claude Code in the tab writes now, found from its
    /// process as the footer finds it (its own `CLAUDE_CONFIG_DIR` or
    /// `HOME`) — what confirms a wall read on its screen before the turn-end
    /// policy acts on it ([`wall_confirm`], H6-HARNESS-1). `None`: no
    /// transcript this host can read (no Claude Code, its process not
    /// registered yet, its conversation not written yet, two of its id and
    /// none in its project's own place, a file that cannot be read): then a
    /// wall read under a tool's call is not acted on, and any other wall as
    /// the screen reads it. The default reads none.
    fn transcript_tail(&self) -> Option<String> {
        None
    }

    /// The reset a limit's words name, placed in the zone only the host knows:
    /// `text` is the notice's reset words ([`Phase::Limited`]'s `reset`), `now`
    /// unix seconds. Codex prints its `try again at` time in its own process's
    /// zone, which a `TZ` exported in the tab's shell may set and the window's
    /// process never sees (`crate::harness::upgrade_drive::codex_wall_reset`).
    /// `Some(Some(unix))`: placed; `Some(None)`: the words cannot be placed,
    /// so no reset (never guessed); `None`: the host places nothing and the
    /// loop reads the words itself ([`limit::reset_at`]). The default: `None`.
    fn place_reset(&self, text: &str, now: i64) -> Option<Option<i64>> {
        let _ = (text, now);
        None
    }

    /// The loop's next try, told to the host (`Some`) or gone (`None`): the
    /// deadline of the API wall's retry wait
    /// ([`Session::wait_for_next`]'s `turn_end_due`, a
    /// [`TurnEndAction::WaitUntil`] with `retry`), as a unix second,
    /// and why. Told when the deadline changes or the wall clock and the
    /// loop's (which stops while the machine sleeps) drift apart, never on
    /// every step, so a tab can show `→ 14:05 · 3m00s` without ticking.
    /// `None` when no retry wait stands (the try is being typed, the worker
    /// works, the point waits for another reason, or the loop ended).
    /// Reporting only. The default ignores it.
    fn waiting(&self, plan: Option<WaitPlan>) {
        let _ = plan;
    }

    /// AN ACT OF THE HARNESS'S, TOLD AS IT IS TAKEN
    /// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.2.3): one call per
    /// ledger row that names an [`Act`](crate::harness::acts::Act), from the
    /// loop's single ledger writer, so the window can light its Autopilot
    /// chip for a moment. The ledger stays the record of every act; this is a
    /// view of one. Reporting only. The default ignores it, and so a CLI
    /// `watch` or `supervise`, which has no host, pays nothing.
    fn act_taken(&self, act: crate::harness::acts::Act) {
        let _ = act;
    }

    /// THE TASK'S FACE, TOLD AS IT MOVES (§9.4.2): open, landed, done, spent
    /// or theirs ([`TaskFace`](crate::harness::acts::TaskFace)), told once at
    /// the loop's start and then on each change, so the window's chip and
    /// `status task=` say how a landing ended. Reporting only: nothing the
    /// loop decides reads it back. The default ignores it.
    fn task_face(&self, face: crate::harness::acts::TaskFace) {
        let _ = face;
    }

    /// The line a person runs to resume the agent's own conversation after a
    /// restart (`claude --resume <id>` with its launch flags, a `--model` the
    /// conversation's record marks failed left off,
    /// [`crate::harness::upgrade_drive::resume_command_kept`]), or `None` when
    /// the host read none. Named where this host would not carry the explicit
    /// restart ([`Self::restart_offered`]); never `claude --continue`, which picks
    /// the directory's newest conversation, possibly a sibling tab's. The
    /// default: none.
    fn resume_command(&self) -> Option<String> {
        None
    }

    /// What this host measures of the agent's route to its API ([`Reach`]):
    /// `Down` only on a definite failure, `Up` only on a completed verified
    /// handshake, `since` the start of the measure's run (so only a new run is
    /// an edge). The loop asks only at a wall that never reached the API, was
    /// cut off, or refused a certificate or proxy ([`Session::reach_seen`]),
    /// at the point and each step of its wait: asking keeps the host's probe
    /// running, so it is never asked elsewhere. The default measures nothing
    /// ([`Reach::Unknown`], the time ladder).
    fn reach(&self) -> Reach {
        Reach::Unknown
    }

    /// What this host measures of the agent's memory
    /// ([`super::memory::AgentMemory`]): the agent's process-group footprint,
    /// the machine's memory, pressure and swap rate, and the largest other
    /// supervised agent, read at most every [`super::memory::SAMPLE_EVERY`],
    /// off the window's thread, spawning nothing. Judged against `[harness]
    /// memory_ceiling` ([`super::memory::judge`]): over the ceiling is a
    /// warning, said once an episode and on each verdict change
    /// ([`Self::warn`]); only a measure that
    /// [`super::memory::MemoryOver::acts`] (the machine in trouble, this agent
    /// the largest) is, at an idle point, the memory wall
    /// ([`super::policy::turn_end::TurnEndReading::wall`]). The default
    /// measures nothing.
    fn memory(&self) -> Option<super::memory::AgentMemory> {
        None
    }

    /// A warning for a person from the session's supervisor, nothing asked or
    /// held back: shown as a warning row in the window's Messages band
    /// ([`super::memory::MemoryOver::warning`]). The default: [`Self::inform`].
    fn warn(&self, text: &str) {
        self.inform(text);
    }

    /// A point the host asked for was withheld, and by which guard
    /// ([`Guard`]): said only while the host [`Self::wants`] a point and only
    /// when the guard changes, so a long turn is one call. A running
    /// foreground turn is `NoBackgroundWait`. Reporting only (the window's host
    /// records it on the upgrade's record). The default ignores it.
    fn withheld(&self, guard: Guard) {
        let _ = guard;
    }

    /// A RATING SURVEY THE LOOP CANNOT CLOSE, or its end (`None`): told once when
    /// it begins ([`StuckSurvey`], with the sentence the journal carries) and once
    /// when the card leaves. Owner rule: it is a fact the owner may
    /// act on (press `0` in that tab) but not a reason to mark the tab — the
    /// card blocks nothing the worker does — so the loop writes no attention for
    /// it. The host keeps a record and, once the card has stood an hour, one
    /// dismissable line of the exceptions digest. Told only while the loop is
    /// unattended (no manager watches it). The default ignores it: the journal's
    /// `WAITING`/`CLEARED` lines say it anyway.
    fn survey_stuck(&self, stuck: Option<(StuckSurvey, &str)>) {
        let _ = stuck;
    }

    /// A POINT ONLY A PERSON CAN ANSWER stands, or has ended: the turn-end
    /// policy's [`Exception`] — why the machine cannot answer it and the one
    /// action that does — told once per change, `Some` when it appears (or
    /// changes) and `None` when it is over (the worker works again, the
    /// condition left the screen, the loop ended). The same words are the
    /// journal's `EXCEPTION …` line. A host that keeps a digest of what needs
    /// a person's look files a `Later` one here, and a host that can
    /// interrupt the owner asks a `Now` one that no escalation of the loop's
    /// carries ([`Exception::escalated`]); nothing about the loop waits on
    /// the answer. The default keeps none: the journal line is the record.
    fn exception(&self, standing: Option<&Exception>) {
        let _ = standing;
    }

    /// THE STALL CANARY'S SECOND RUNG ([`super::wall_memory`]): a screen no
    /// reader names has stood, the agent's own title resting on the idle glyph,
    /// for `Some(minutes)` — the screen has read busy on the agent's own
    /// background work alone (told once when the stall passes 90 minutes), or
    /// it is a dialog with no composer, box or busy row whose words have not
    /// changed (30 minutes) — and `None` when it is over (the screen moved,
    /// the agent works, the loop ended). A fault of aterm's own reading, not a
    /// wait: the host keeps a record and an error line. The third rung (three
    /// hours; an hour for a dialog) is an [`Exception`] of
    /// `ExceptionKind::Stalled` or `ExceptionKind::Dialog` through
    /// [`Self::exception`]. Nothing is typed either way. The default ignores
    /// it: the journal's `STALLED` line says it anyway.
    fn stall_unread(&self, minutes: Option<u64>) {
        let _ = minutes;
    }

    /// WHAT THE AGENT'S OWN SCREEN SAYS OF ITS ACCOUNT'S USAGE ([`UsageSeen`]:
    /// usage credits carrying the session, the vendor's warning that a window
    /// is filling), told from every read the loop makes, busy reads included:
    /// once per change, and again every
    /// [`crate::harness::accounts::CREDITS_RETELL_S`] while it says something.
    /// FOR THE RECORD (accounts v2 §3.3): credits are work going on, never a
    /// wall, and nothing the loop decides reads this — the host keeps it for
    /// the account and its digest. The default ignores it.
    fn usage_seen(&self, seen: &UsageSeen) {
        let _ = seen;
    }

    /// WHAT THIS HOST ADVISES ABOUT THE SESSION'S ACCOUNT at `point`
    /// ([`AccountAdvice`]; `docs/DESIGN-claude-accounts-2026-10-01.md` §6):
    /// the roster's judgment — move to another of the owner's Claude Code
    /// accounts, or no account is usable before an instant — for the
    /// turn-end policy to decide on
    /// ([`super::policy::turn_end::TurnEndReading::account`]). `None` is
    /// "stay": no roster, a session the roster does not list, nothing due.
    ///
    /// ASKED ONCE A DECISION, after the point is folded into the policy's
    /// memory, and only where the answer can decide something
    /// ([`super::policy::turn_end::TurnEndState::account_point`]): the reading
    /// of one point is built several times while it is decided and carried
    /// out, and the answer is kept for all of them — a host may read its
    /// roster and state file here. A wait that runs out decides the point
    /// again and asks again: that is how a session waiting on every account
    /// learns one is usable. [`AccountPoint::Wall`] carries what the SCREEN
    /// reads; that the limit is this process's own is the host's to check
    /// before it rotates for it (from a limit that is not, a host advises
    /// only what it would at a plain turn end: following the account the
    /// others are on).
    ///
    /// A HOST WHOSE EVIDENCE LAGS THE SCREEN — the limit's row not in the
    /// transcript yet — LEARNS TO LOOK AGAIN BY BEING ASKED AGAIN: it
    /// answers [`AccountAdvice::Wait`] for an instant a moment away, the
    /// wall's wait is cut to it (never nearer than a minute), and the point
    /// decided then asks this once more. How many times is the host's to
    /// count; with no such answer the next ask is the wall's own next
    /// decision, which may be its reset.
    ///
    /// Never asked under `[harness] accounts = "off"`. The default advises
    /// nothing, and with it every decision is the one made before accounts
    /// existed.
    fn account(&self, point: AccountPoint) -> Option<AccountAdvice> {
        let _ = point;
        None
    }

    /// THE ACCOUNT THIS SESSION WAS MOVED TO SERVED A TURN: the first turn
    /// asked after a relaunch this host made for
    /// [`super::policy::turn_end::Restart::Account`] ran and ended with no
    /// wall ([`super::policy::turn_end::TurnEndState::account_answer`]) —
    /// or, a bad answer of the move told already, the worker worked past it
    /// with no wall (the limit lifted, the login back), where the move ends.
    /// Told once a move; the host records the account proven, and others may
    /// follow. Never told under `[harness] accounts = "off"`, like
    /// [`Self::account_answered`]. The default ignores it.
    fn account_served(&self) {}

    /// THE ACCOUNT THIS SESSION WAS MOVED TO ANSWERED WITH A WALL OR A LOST
    /// LOGIN instead ([`AccountBad`]): told once a move, BEFORE the same
    /// point asks [`Self::account`] — so a host that strikes the account
    /// here advises the next one, or the way back, in its very next answer.
    /// A usage limit is told only after a turn was asked under the new
    /// login, and only from the screen: the host checks it against its own
    /// evidence (the limit row this process wrote) before it strikes
    /// anything. The default ignores it.
    fn account_answered(&self, bad: AccountBad) {
        let _ = bad;
    }

    /// A STAMP THAT MOVES WHEN WHAT THIS HOST WOULD ADVISE MAY HAVE CHANGED
    /// with nothing of the session's own screen moving ([`Self::account`]):
    /// the owner's `aterm harness accounts use`, a check that found a login
    /// signed in, an account another session proved or struck. A session
    /// WAITING at a usage limit is decided again only when its screen moves
    /// or its wait runs out — which may be days off — so the loop reads this
    /// at the top of each step of a wait that stands on the host's advice
    /// ([`Session::account_edge`]) and decides the point again when it has
    /// moved since the session last read it — and a session whose move this
    /// host refused for good asks again at that news, where a refusal with
    /// none after it is not asked again
    /// ([`super::policy::turn_end::TurnEndState::account_news`]). The first
    /// value a loop reads is no news to it (the count is the host's, since
    /// it started). It must be
    /// cheap (one atomic read): it is asked every step, and at every
    /// decision. The default never moves, and with it no wait is decided a
    /// moment sooner than before accounts existed.
    fn account_stamp(&self) -> u64 {
        0
    }

    /// THE RENEWABLE LOGIN THE SESSION RUNS ON
    /// ([`super::policy::turn_end::TurnEndReading::login_label`]): the
    /// roster label of a row with an email that its process's login is —
    /// the row's renewed login, or one a renewal superseded. Asked at every
    /// reading of a point under `[harness] logins`; it must be cheap where
    /// no roster lists such a row. `None` (the default): every lost login is
    /// decided as it was before renewal existed.
    fn login_label(&self) -> Option<String> {
        None
    }

    /// WHETHER THE SESSION'S PROCESS HOLDS A LOGIN A RENEWAL REPLACED
    /// ([`super::policy::turn_end::TurnEndReading::login_superseded`]): its
    /// renewable row ([`Self::login_label`]) is bound to the renewed login,
    /// and the process is still on the one before it. Asked only at a lost
    /// login on the screen of a session on a renewable login: at every
    /// reading of such a point (a decision's, and each read for the loop's
    /// books after its act or restart), and, while a point decided on
    /// `false` stands, once each time [`Self::account_stamp`] moves (a
    /// renewal made then decides the point again). `false` (the default):
    /// the process holds its row's own login.
    fn login_superseded(&self) -> bool {
        false
    }

    /// CLAUDE CODE SAID THE STORED LOGIN ENDS IN `days` DAYS
    /// ([`super::policy::turn_end::TurnEndReading::login_notice`]): told
    /// once per value a loop reads, under `[harness] logins`. A host that
    /// keeps the logins' forecast records it for the login the process holds
    /// — only where that is the row's own renewed login. The default ignores
    /// it.
    fn login_notice(&self, days: u32) {
        let _ = days;
    }

    /// CLAIM THIS SESSION'S ACCOUNT'S ONE `/usage` PROBE
    /// (`docs/SPEC-claude-accounts-probe-2026-10-01.md`, P23, P27). `Ok`:
    /// the loop types `/usage` into the session NOW, reads the Settings
    /// panel it opens and closes it ([`Session::usage_probe`]) — and tells
    /// what it read, exactly once, through [`Self::account_probed`].
    ///
    /// ASKED LAST, after every gate of the loop's own has passed — the point
    /// is a plain idle turn end whose turn was read live, this host's advice
    /// for it was [`AccountAdvice::Probe`], the composer is empty with nobody
    /// at it, the server fences a write three ways, the agent draws on the
    /// alternate screen — so a claim is never wasted on a point the loop
    /// then declines. What the loop CANNOT know is the host's to check here,
    /// under its roster's lock: that `[harness] accounts` is `early` (the
    /// loop cannot tell it from `wall`), that the roster still says the read
    /// is due, and that no session of any account claimed one within the
    /// roster's spacing (`crate::harness::accounts::claim_probe`).
    ///
    /// THE DEFAULT CLAIMS NOTHING, and with it nothing is ever typed for a
    /// probe: no roster, a host that keeps none, `accounts = "off"` or
    /// `"wall"` — every byte the loop writes is the one it wrote before the
    /// probe existed.
    ///
    /// `Ok`: the claim is this session's, and the loop types. `Err`: the
    /// host's ONE WORD for why not — short, stable, no account's name — which
    /// the loop puts on its line about the probe it did not run (`SKIPPED …
    /// gate=claim host=<word>`: [`Session::probe_shut`]), so a reader of the
    /// journal need not find the host's log to learn which refusal it was.
    fn account_probe(&self) -> Result<(), &'static str> {
        Err("no-accounts")
    }

    /// WHAT THE CLAIMED PROBE READ ([`Self::account_probe`] answered `Ok`):
    /// `Ok`, the settled panel's windows as the roster's sample
    /// (`crate::harness::usage::panel_sample`: the loop placed the resets on
    /// its own clock and zone table; `by` is the recorder's to stamp), or
    /// `Err`, why nothing was read ([`ProbeMiss`]). Told ONCE a claim, and
    /// BEFORE the panel's Escape, so a close that drags on never costs the
    /// sample. The host records the sample only for the process the claim
    /// was made for; the loop names no account. The default ignores it: the
    /// journal's `PROBED` line says it anyway.
    fn account_probed(&self, read: Result<&crate::harness::accounts::Sample, ProbeMiss>) {
        let _ = read;
    }

    /// MAY THIS SESSION TAKE ITS ACCOUNT'S LIMIT RESET
    /// (`docs/SPEC-claude-accounts-resets-2026-10-02.md`, R7 to R10)? Asked
    /// LAST, after every gate of the loop's own and the person's hold,
    /// immediately before the first key on the usage-limit dialog's grant row
    /// — and once more before the confirmation's Enter, where anything but
    /// [`ResetWord::Take`] keeps the reset. `ask` is the row as read: its
    /// count, its day, and that day's first second on the loop's clock.
    ///
    /// What the loop CANNOT know is the host's to check here, under its
    /// accounts lock: that `[harness] limit_reset` is on (read live), that the
    /// process runs on the default login and no other, that the wall is this
    /// process's own and one a grant clears, that it qualifies (a wall more
    /// than a day long, or a grant whose last day could end within one), and
    /// that the ONE claim for this wall is this session's
    /// (`crate::harness::accounts::claim_reset`). [`ResetWord::Take`] carries
    /// what the confirmation is then held to (`ResetAsk::go`);
    /// [`ResetWord::Again`] is asked again in a moment; [`ResetWord::No`]
    /// names why not in one word ([`ResetNo::word`]), and the dialog is then
    /// answered as it was before resets existed.
    ///
    /// THE DEFAULT TAKES NOTHING, and with it no row is taken and no Enter
    /// of a reset is ever written: no host, a host that keeps no accounts,
    /// the key off. (The one key a loop may still write for a reset is the
    /// Escape of a step an earlier loop left open.)
    fn reset_take(&self, ask: &ResetAsk) -> ResetWord {
        let _ = ask;
        ResetWord::No(ResetNo::NoHost)
    }

    /// THE STEP A TAKEN RESET OPENED HAS ENDED ([`Self::reset_take`] answered
    /// [`ResetWord::Take`], and the dialog is gone): `end` says whether the
    /// confirmation's Enter was written, why the loop kept it where it did,
    /// and the offer an exactly-read confirmation showed. The host reads the
    /// vendor's ONE answer line from the transcript — never the screen — and
    /// records it for the claim it holds: [`ResetOutcome::Pending`] while no
    /// line is written yet (asked again, the last time with `end.last`), else
    /// what the line says. Only `Limits reset · …` is a reset spent.
    ///
    /// The default reads nothing ([`ResetOutcome::Unread`]): the loop then
    /// owes the session one continuation, as after any reset that was kept.
    fn reset_told(&self, end: &ResetEnd) -> ResetOutcome {
        let _ = end;
        ResetOutcome::Unread
    }
}

/// WHY A CLAIMED `/usage` PROBE RECORDED NO SAMPLE
/// ([`IdleHost::account_probed`]): where the step ended
/// ([`Session::usage_probe`]). Reporting only — the host logs the word and
/// lets its claim go; nothing decides by it.
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
    /// The panel's last row is under the fold of the grid: what stands
    /// right above it cannot be read, so nothing is said of the figures
    /// that are visible.
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

/// Which rating survey the loop cannot close ([`IdleHost::survey_stuck`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StuckSurvey {
    /// A card the harness will not press: a shape nobody has words for, or a
    /// question row no guard can fence.
    Withheld,
    /// The harness pressed `0` again and again and the card stayed.
    Unclosed,
}

/// `supervise`'s knobs (and `watch`'s: the same loop).
///
/// Every policy switch is [`Self::policy`], the `[harness]` table
/// ([`SupervisorConfig`]): the host sets it whole ([`Self::hosted_with`]), and
/// the CLI's flags may only take power away from it.
#[derive(Debug, Clone, Default)]
pub struct SuperviseOpts {
    /// The wall-clock budget.
    pub max: Duration,
    /// `python3 <script>` globs that count as reads (empty = the default list).
    pub python_allow: Vec<String>,
    /// A notes file to append one line per action to.
    pub notes: Option<PathBuf>,
    /// `watch --report`: an idle, question or limited point's EVENT line
    /// carries `complete=<0|1> rows=<n>` of `report` before its summary.
    pub report: bool,
    /// `--context-warn`: the percentage of context left
    /// ([`super::phase::context_left`]) at or below which the loop says `EVENT
    /// context seq=<n> <v>% until auto-compact`, once a descent; afterwards
    /// the indicator gone from a framed screen with no box (or rising by
    /// [`COMPACTED_RISE`]) is `EVENT compacted seq=<n>`. `0` disables both
    /// (the default here; the CLI's is 10). Armed per loop: a compaction
    /// between two `supervise` runs is not seen.
    pub context_warn: u8,
    /// `--journal`: a file to append one JSON object to for every line the
    /// loop prints — and, for `supervise`, for every line `watch` would have
    /// printed for what it decides silently ([`super::journal`]).
    pub journal: Option<PathBuf>,
    /// The approval ledger a hosted loop keeps ([`Session::run_hosted`]),
    /// named by its host (`<state>/drive/<sid>.jsonl`). `None`: no ledger
    /// unless [`Session::set_approval_ledger`] gave one; the loop never falls
    /// back to the process's state directory.
    pub ledger: Option<PathBuf>,
    /// `--mail`: park the mail lane ([`super::mail::Lane`]) on the manager's
    /// inbox beside the loop. Every delivery prints `MAIL id=<n> off=<o>
    /// from=<sid> kind=<k> len=<n> [re=<o>]`; an idle point is held
    /// (`Session::hold_for_report`, `idle_grace` at most) for the worker's
    /// `report` of the same turn and printed as one line, `EVENT turn seq=<n>
    /// report=<id> rows=<n> <summary>` when it came
    /// ([`MailIn::is_this_turns`]), else `EVENT idle-no-report seq=<n>
    /// <summary>`. `None` leaves every line as it was.
    pub mail: Option<MailOpts>,
    /// The owner's policy ([`SupervisorConfig`], full power by default):
    /// what a box may be answered with (`approve`), the trust roots, the
    /// rating surveys (`dismiss_surveys`: press `0` on the session survey, or the
    /// long-context, plugin or memory card, with the guarded press, `key
    /// if=^●.How.is.Claude.doing 0`, and say `DISMISSED survey seq=<n>` — `0`
    /// only, ever: a rating is a person's to give), the
    /// answers, the continuation, retry and limit switches, and how long a
    /// person's keystroke keeps the loop's hands off the session.
    pub policy: SupervisorConfig,
    /// Behind another holder's supervisor claim, end the loop (`Err` starting
    /// with [`CLAIM_HELD`], naming the holder) instead of watching behind it.
    /// The in-GUI host sets it ([`Self::hosted`]) and restarts a loop when the
    /// claim is released or lapses; `watch` leaves it off (`WATCHING …`). The
    /// loop's own claim is the only one either way.
    pub yield_when_held: bool,
    /// The host's part at idle points ([`IdleHost`]): takes the live
    /// upgrade's step and a relaunched agent's continuation in place, and says
    /// when the upgrade owns turn ends. `None` for `watch` and `supervise`.
    pub idle_host: Option<Arc<dyn IdleHost>>,
}

impl SuperviseOpts {
    /// The in-GUI host's loop: no time budget, the approval policy at full
    /// power, the survey dismissed, every turn end decided by the turn-end
    /// policy, the context indicator watched. Outages are ridden out, an
    /// unanswerable point is escalated to the worker's attention, and mail
    /// goes to a manager only while the fabric is connected. No `--mail` lane.
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

    fn allow(&self) -> Vec<String> {
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
/// The hosted loop's `context_warn` (the CLI's default).
const DEFAULT_CONTEXT_WARN: u8 = 10;
/// How many rows a screen read asks for when the server can tail — the
/// server's own agent-verdict cut (aterm-gui's `presence::CLASSIFY_ROWS`).
const TAIL_ROWS: usize = 40;

/// Whether a `tail=` read cannot stand for the screen and is taken again whole.
/// The grid must be taller than the tail (`first > 0`), and then either:
///
/// * the box `reader` (the session program's) finds has its title row off the
///   rows read ([`aterm_phase::prompt::PromptV2::head_off_screen`]), so a box
///   the pane shows entire is decided by its title; or
/// * the tail does not hold the live rows at all: the cursor's row is above
///   it, or every row of it is blank (a dialog drawn wholly above the tail in
///   a tall pane). This test reads the reply alone, no grammar.
///
/// Every other read keeps the tail — a box or a composer at the foot of the
/// pane, the cursor on it, is read once — unless it ends on a blank row
/// under drawn rows and its reader decides nothing from it or an idle
/// composer ([`drawn_above_tail`]), or is drawn to its last row and holds a
/// blank GAP the live zone counts as one row ([`gap_above_tail`]).
fn tail_misses_the_live_rows(reader: &dyn ScreenReader, screen: &Screen) -> bool {
    screen.first > 0
        && (screen.cursor_row < screen.first
            || screen.rows.iter().all(|r| r.trim().is_empty())
            || reader
                .prompt(&screen.rows)
                .is_some_and(|p| p.head_off_screen))
}

/// Whether a `tail=` read that ends on a BLANK row cannot stand for the
/// screen: rows above it (`first > 0`), its last row blank — so what is
/// drawn ends above the grid's last row and the screen's live zone, its last
/// [`TAIL_ROWS`] drawn rows and the blank rows under them
/// ([`aterm_phase::live_zone_start`], the zone the server's agent verdict
/// reads), begins above the tail — and the session's reader, handed the tail
/// and the cursor on it ([`ScreenReader::read_at`]), decides no phase from it
/// with authority or decides IDLE. Such a read is taken again whole and cut
/// to the live zone ([`Session::live_zone`]).
///
/// Claude Code's inline renderer draws its REPL at the top of the pane and
/// leaves the rows below it blank until the transcript fills it: on a pane
/// taller than the tail, the tail can hold the prompt box's bottom rule, its
/// footer and blank rows, or its caret row under a top rule the tail cut, and
/// its `idle` was no evidence, so the host's idle-point step never ran
/// (measured at 150x50, the REPL on rows 8-11). It can also hold the WHOLE
/// composer, the cursor on its caret, and then its reader DOES decide — but
/// an idle composer is the one reading whose meaning lies in the rows above
/// it: the worker's last words, a rating card, a wall's notice, a question.
/// Read from the tail alone, a rating card above it (Claude Code 2.1.286,
/// 144x53: card on rows 11-12, top rule on row 14, tail from row 13) was
/// journaled as `EVENT idle` and left up for good — blind whenever the head
/// row lies above the tail and the top rule at or under its first row. A
/// BOX the reader decides — an approval, a question, with the cursor on it
/// — or a RUNNING turn is acted on from the rows the tail holds, pressed or
/// waited on, and stands, read once
/// ([`tail_misses_the_live_rows`]: a subagent rm box in the tail of a fresh
/// 62-row pane, blank rows under it); the idle look that follows it reads
/// the zone.
fn drawn_above_tail(reader: &dyn ScreenReader, screen: &Screen) -> bool {
    if screen.first == 0 || screen.rows.last().is_none_or(|r| !r.trim().is_empty()) {
        return false;
    }
    let read = reader.read_at(&screen.rows, screen.cursor_index(), None);
    !read.phase_authoritative || read.phase == Phase::Idle
}

/// Whether a `tail=` read drawn to its last row cannot stand for the screen
/// because the live zone begins above it: rows above the tail (`first > 0`),
/// and the tail holds a blank gap ([`aterm_phase::BLANK_GAP`] rows or more,
/// counted as one row by [`aterm_phase::live_zone_reaches_above`], the same
/// walk the server's agent verdict takes). Such a read is taken again whole and
/// cut to the live zone ([`Session::live_zone`]). A gap the tail's top row cuts
/// is not seen from the tail; a tall dialog at the foot of a pane is read from
/// its tail, once.
///
/// Example: a fullscreen pane with an API error near the top, a long blank
/// gap, and the composer pinned to the bottom rows. The tail alone reads an
/// ordinary idle point and would miss the wall.
fn gap_above_tail(screen: &Screen) -> bool {
    screen.first > 0 && aterm_phase::live_zone_reaches_above(&screen.rows, TAIL_ROWS)
}

/// The longest single wait, so the budget — and the host's request for an
/// idle point ([`IdleHost::wants`]) — is re-checked between waits.
const WAIT_STEP: Duration = Duration::from_secs(20);
/// The step of the waits a host's request is read between (a turn's, and the
/// wait for the next one, [`Session::wait_for_next`]): [`WAIT_STEP`], never
/// longer than the live upgrade's settle as this process runs it
/// ([`crate::harness::upgrade::quiet_s`]), the settle the upgrade's gate and a
/// break of background work ([`background_settle`]) ask for anyway. In every
/// shipped binary the two are equal (asserted below), so this is
/// [`WAIT_STEP`]. A development build that shortens the settle (the live
/// suite) would otherwise hold a request a whole 20 s step its settle does
/// not ask for.
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
const BOX_SETTLE: Duration = Duration::from_millis(300);
/// The server's agent verdict words (`status agent=`: `busy`, `prompt`,
/// `question`, `idle`, `survey`, `wall`, `unknown`, `-`), all of which an
/// `await agent` may name — every one but the point's own
/// ([`Session::agent_wake`]): here an idle point's.
const AGENT_WORDS_BUT_IDLE: &str = "busy,prompt,question,survey,wall,unknown,-";
/// [`AGENT_WORDS_BUT_IDLE`] for a question: every word but `question`.
const AGENT_WORDS_BUT_QUESTION: &str = "busy,prompt,idle,survey,wall,unknown,-";

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
/// The longest idle wait on a screen without the composer frame before it is
/// read again, so a box that replaces a shell prompt is seen promptly.
const WRITING_CAP: Duration = Duration::from_secs(3);
/// The longest wait on the busy row armed before any screen was read, over
/// every row ([`BusyWait::step`]): as [`WRITING_CAP`], a wait that cannot
/// tell what it waits on is read again within a few seconds.
const UNSCOPED_CAP: Duration = WRITING_CAP;
/// How many points higher than its last reading the context indicator must
/// read, after `EVENT context`, for the worker to have compacted — a
/// compaction replaces the history with a summary and frees most of the
/// context; a smaller rise is not taken for one. Not measured: how far the
/// reading rises when `/model` switches the worker to a larger context.
const COMPACTED_RISE: u8 = 30;
/// How long the fallback press waits for the worker to take the digit before
/// looking for one that landed in the composer.
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
const RESULT_ROWS: usize = 28;
/// How many characters of a command, a said row or a notice one `watch` line
/// carries.
const LINE_CHARS: usize = 160;
/// How many non-blank rows of the transcript, up to the last thing said,
/// make a review point's words ([`point_key`]) — and, above a box the reader
/// could not read whole (its head off the screen, a question named by its
/// footer or options alone), that box's ([`box_key`]). Above a box read whole
/// only the letters above its echo count ([`ABOVE_LETTERS`]).
const KEY_ROWS: usize = 8;
/// How many letters of the transcript above a box read whole — above the
/// pending call's echo ([`before_the_echo`]) — count in its key
/// ([`box_key`]): enough that output the worker appended before the same box
/// asked again sets them apart, and few enough to stand within the rows a
/// short pane keeps above a box (92 above the echo of the measured 80x24 rm
/// box, `cap-taken-box-1`), so a row scrolling in at the pane's top is no
/// change.
const ABOVE_LETTERS: usize = 64;

/// The exit code `supervise`, `watch` and `await-turn` end with on a spent
/// budget.
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
/// does not count sleep). A clock stepped BACK never triggers it.
pub(super) fn slept_through(wall_delta_s: i64, mono_delta_s: i64) -> bool {
    wall_delta_s - mono_delta_s > SLEPT_THROUGH_S
}
/// How far off the reset a limit names can be — a weekly limit's, a week. A
/// notice read as further off than this is misread, and stretches no budget
/// ([`Session::extend`]): a watcher never extends itself by a year on a
/// misreading.
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

/// The most one attention entry of this loop's holds: the server's cap for a
/// keyed entry (200 bytes), not the bare field's 256.
const ATTENTION_BYTES: usize = aterm_types::control_verbs::META_ATTENTION_KEYED_MAX;
/// The prefix a limit episode's attention opens with
/// ([`Session::open_episode`]), the one [`Session::close_episode`] unsets. The
/// entry is owned by its key ([`ATTENTION_OWNER`]); a person's entry is theirs.
pub const ATTENTION_PREFIX: &str = "limited:";
/// How much of the box's first line the attention text carries, in terminal CELLS — a
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
/// A refusal not met for this long is NEWS when it comes back
/// ([`Session::refusal_news`]): one that persists is met at every retry,
/// seconds apart (the back-off is [`REFUSAL_PAUSE_MAX`] at most, the act
/// decided again a moment later); one met again after a wait of minutes —
/// a wall's reset, a back-off's rung — is a new appearance.
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
fn box_anchor(reader: &dyn ScreenReader, rows: &[String]) -> Option<String> {
    let (header, esc) = reader.prompt(rows)?.span;
    let row = rows[header + 1..esc]
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
}

impl BusyWait {
    /// How long one wait may run: `step`, or [`UNSCOPED_CAP`] at most when
    /// armed before any screen was read, over every row: before a read, a
    /// running turn's busy row and a dead process's footer look alike. The
    /// read the cap brings arms the next wait over the live zone.
    fn step(&self, step: Duration) -> Duration {
        if self.unscoped {
            step.min(UNSCOPED_CAP)
        } else {
            step
        }
    }
}

/// Where the live zone of `screen` starts ([`ScreenReader::busy_zone_start`]),
/// as an index into its rows, and the grid rows a busy-row wait is armed over:
/// from there to the grid's last row. `None` (every row) only when the zone
/// starts at grid row 0 of a whole read. A `tail=` read is always scoped to the
/// rows it returned, since rows above `first` were never seen. Scoping keeps a
/// dead process's busy footer above a new launch card from holding the wait for
/// a whole step. The arming busy row is in the span by construction (`busy_up`
/// over `rows[at..]`).
fn live_rows(reader: &dyn ScreenReader, screen: &Screen) -> (usize, Option<(usize, usize)>) {
    let n = screen.rows.len();
    let at = reader.busy_zone_start(&screen.rows).min(n);
    let span = (at < n && screen.first + at > 0).then(|| (screen.first + at, screen.first + n - 1));
    (at, span)
}

/// Whether the loop keeps Codex's own state for a session running `p`: the
/// footer's model, effort and goal ([`Session::note_codex_screen`]) and the
/// thread's records ([`Session::codex_records`]).
fn reads_codex_state(p: Program) -> bool {
    match p {
        Program::Codex => true,
        Program::Claude | Program::Agy | Program::Muse | Program::Generic => false,
    }
}

/// Whether a reset on `p`'s wall is placed on the screen's print of it
/// ([`limit::place_reset_with`], a bare clock time re-placed on a new print).
/// Codex's words keep their first placement ([`limit::reset_at_with`]), and so
/// do agy's and Muse Code's, whose walls name no reset that was measured.
pub(crate) fn places_resets_on_print(p: Program) -> bool {
    match p {
        Program::Claude | Program::Generic => true,
        Program::Codex | Program::Agy | Program::Muse => false,
    }
}

/// Whether one wait saw the content move past a seq ([`Session::moved_past`]).
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
    /// The pause before the next try to reach the session: it keeps doubling
    /// across the outage's ride-outs, so one that keeps coming back is tried
    /// less and less often, not every half second.
    pause: Duration,
}

/// How the shared loop ended.
enum End {
    /// The review stopped the loop at this point (`supervise`), with how
    /// `--mail` settled it when it was an idle one.
    Stopped(Turn, Option<Fold>),
    /// The budget ran out; the last screen read.
    Timeout(Turn),
    /// The hosted loop's stop flag was set ([`Session::run_hosted`]).
    Quit,
}

/// The loop ended because the session is gone (`ERR exited`, `ERR no such
/// session`): there is no attention left to clear.
fn session_gone(end: &Result<End, String>) -> bool {
    matches!(end, Err(e) if e.contains("ERR exited") || e.contains("no such session"))
}

/// `watch`'s last line and exit code for how its loop ended.
fn watch_end_line(end: Result<End, String>) -> (String, u8) {
    match end {
        Ok(End::Timeout(_)) => ("TIMEOUT".to_string(), EXIT_TIMEOUT),
        // `Lines` never stops at a review point; kept for the match.
        Ok(End::Stopped(..)) => ("EXIT stopped at a review point".to_string(), 1),
        Ok(End::Quit) => ("EXIT stopped".to_string(), 0),
        Err(e) => (format!("EXIT {}", exit_reason(&e)), 1),
    }
}

/// `now + max`; a budget no clock can reach ([`UNBOUNDED`]) is a century.
fn deadline_after(max: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(max)
        .or_else(|| now.checked_add(Duration::from_secs(100 * 365 * 86_400)))
        .unwrap_or(now)
}

/// How `--mail` settled an idle point (`Session::hold_for_report`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// The worker's `report` of this turn came within the window: the point
    /// prints as `EVENT turn seq=<n> report=<id> rows=<n> <summary>`.
    Report { id: u64, rows: usize },
    /// None came within the grace: `EVENT idle-no-report seq=<n> <summary>`.
    NoReport,
}

/// The loop's side of the mail lane ([`SuperviseOpts::mail`]).
struct MailIn {
    /// The worker's `report`s, as the lane hands them over.
    rx: Receiver<Delivery>,
    /// Reports that came before an idle point, kept until the point.
    pending: Vec<Delivery>,
    window: Duration,
    grace: Duration,
    /// The lane ended (its `MAIL off` line said why): no report can
    /// come, so an idle point prints as without `--mail`.
    gone: bool,
    /// Where the turn in progress begins, at the latest: the moment the last
    /// point a turn ended on (idle, a question, a limit notice) was handed
    /// over — the loop's start before any. A report from before it is that
    /// turn's, or an older one's, never this turn's.
    floor: Instant,
    /// The first read that saw the worker busy after `floor`: the turn had
    /// begun by then, so a report from after it is this turn's however long
    /// ago it came (a report posted mid-turn by a re-fired Stop, the worker
    /// working on past the window). `None` until such a read.
    busy_at: Option<Instant>,
    /// Whether `floor` is a handed-over point's, not the loop's start.
    handed: bool,
}

impl MailIn {
    /// Whether a delivery is the report of the turn ending at the held point
    /// (`now`): after the turn was seen to begin (`busy_at`), any delivery is;
    /// between a handed-over point and that read, it is the previous turn's
    /// late report; before the turn was seen to begin, only one within the
    /// window is.
    fn is_this_turns(&self, d: &Delivery, now: Instant) -> bool {
        if d.at < self.floor {
            return false;
        }
        match self.busy_at {
            Some(busy) if d.at >= busy => true,
            Some(_) if self.handed => false,
            _ => now.duration_since(d.at) <= self.window,
        }
    }
}

/// How `Session::hold_for_report` settled an idle point: the fold, and the
/// screen it read past the point when the point was superseded under the
/// hold (a prompt, the worker busy again) — the next look's turn.
struct Held {
    fold: Option<Fold>,
    carried: Option<Screen>,
}

/// A client for the loops that run without a mail lane: never called.
pub struct NoLane;

impl Ctl for NoLane {
    fn call(&mut self, _args: &[&str]) -> Result<CtlReply, String> {
        Err("no mail lane".to_string())
    }
}

/// Where every line of a loop goes (`watch`'s stdout, `supervise`'s stderr),
/// each recorded in the journal first. One per loop, shared with the mail
/// lane's thread under a lock so lines never interleave and the journal order
/// is the printed order. The lock is held for one line, taken even when
/// poisoned, and nothing else is locked under it.
struct Sink<'w> {
    out: &'w mut (dyn Write + Send + 'w),
    journal: Journal,
    /// Where the journal's one warning goes (`watch`: stderr); `None` sends
    /// it to `out` (`supervise`, whose lines are stderr already).
    warn: Option<&'w mut (dyn Write + Send + 'w)>,
    /// `watch`'s story lane: the worker's window is told what the loop
    /// decided, one `ctl story` per journaled line that carries a decision
    /// ([`story_of`]). `None` (`supervise`, and a `watch` without one) tells
    /// nothing.
    teller: Option<Teller<'w>>,
}

/// The client `watch` tells the worker's window its story through: a second
/// connection, like the mail lane's, so the loop's own client keeps its outage
/// books to itself; and the session the story is about.
struct Teller<'w> {
    ctl: &'w mut (dyn Ctl + Send + 'w),
    sid: Option<String>,
}

impl Sink<'_> {
    /// Record `line` in the journal (its warning to `warn`, else to `out`) and
    /// tell the window the story it carries, if any. The post is best-effort: a
    /// host without the `story` verb or a lost connection changes no printed
    /// line.
    fn record(&mut self, line: &str, turn: Option<u64>) {
        match self.warn.as_deref_mut() {
            Some(w) => self.journal.record(line, turn, w),
            None => self.journal.record(line, turn, self.out),
        }
        if let Some(teller) = &mut self.teller
            && let Some((verb, text)) = story_of(line)
        {
            let mut args: Vec<&str> = Vec::with_capacity(4);
            if let Some(sid) = &teller.sid {
                args.push(sid.as_str());
            }
            args.push("story");
            args.push(verb);
            if !text.is_empty() {
                args.push(&text);
            }
            let _ = teller.ctl.call(&args);
        }
    }

    /// Record `line`, then print and flush it.
    fn line(&mut self, line: &str, turn: Option<u64>) -> Result<(), String> {
        self.record(line, turn);
        emit(self.out, line)
    }
}

/// The `aterm ctl story <verb> [<text>]` a printed line is told to the
/// worker's window as, `None` for a line that carries no decision (`EVENT
/// idle`, `MAIL`, a `RECONNECT` still being ridden out). The verbs are the
/// GUI's closed set (`approved dismissed reconnected timeout exit compacted
/// warned chose`); the text is the human-facing part, never the command or
/// the question (`APPROVED seq=<n> <command>` is bare `approved`; `CHOSE …
/// policy=<word> …` is `chose <word>`). The text is cut to the wire's 96 bytes
/// at a character boundary.
pub fn story_of(line: &str) -> Option<(&'static str, String)> {
    const MAX: usize = 96;
    let cut = |s: &str| -> String {
        if s.len() <= MAX {
            return s.to_string();
        }
        let mut end = MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s[..end].to_string()
    };
    let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
    Some(match word {
        "APPROVED" => ("approved", String::new()),
        "CHOSE" => (
            "chose",
            cut(rest
                .split_whitespace()
                .find_map(|w| w.strip_prefix("policy="))
                .unwrap_or("")),
        ),
        "DISMISSED" => (
            "dismissed",
            cut(rest.split_once(" seq=").map_or(rest, |(w, _)| w)),
        ),
        "RECONNECTED" => ("reconnected", cut(rest)),
        "TIMEOUT" => ("timeout", String::new()),
        "EXIT" => ("exit", cut(rest)),
        "EVENT" => {
            let (phase, tail) = rest.split_once(' ').unwrap_or((rest, ""));
            match phase {
                "compacted" => ("compacted", String::new()),
                // `EVENT context seq=<n> <v>% until auto-compact`: the reading.
                "context" => (
                    "warned",
                    cut(&format!(
                        "context {}",
                        tail.split_once(' ').map_or(tail, |(_, v)| v)
                    )),
                ),
                _ => return None,
            }
        }
        _ => return None,
    })
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
    /// What the reads since the last turn's end saw of the worker ([`Saw`]).
    /// Kept here, not in one wait's locals, so a busy spell read before a lost
    /// connection still counts after it.
    moved: Saw,
    /// Where the session survey stood at the last look ([`Session::survey`]).
    survey: Survey,
    /// A screen the last look read past its point (the hold's safety net,
    /// [`Session::hold_for_report`]): the next look's turn, if it still shows
    /// a point.
    carried: Option<Screen>,
    /// The screen the last look's press read and handed on ([`Step::Decide`]):
    /// the next look's turn, as read.
    read_back: Option<Screen>,
}

/// What the reads of a turn saw of the worker ([`Session::await_turn_from`]).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Saw {
    /// A read counted as the worker busy: its busy row, or — the screen with
    /// no composer, no box and no busy row, after a wait that saw no
    /// stillness — output still being written (`writing`).
    busy: bool,
    /// A read showed the worker's OWN busy row: its live turn, never a
    /// screen only `writing`. A screen only writing is no agent's read — the
    /// badge's close at a busy read waits for one (a person answered the box
    /// and the worker took it) — and a box redrawn through such a read is the
    /// box that was up, not the same box asked again after work
    /// ([`Session::look`]).
    live: bool,
}

/// The box escalated to the manager, kept so its badge can be cleared when it leaves
/// ([`Session::close_box`]). One ask per REVIEW POINT ([`Session::escalate_point`]): however
/// many looks show the same point, its key ([`Session::review_key`]) never moves with
/// a footer tick, nor — for a box — with a redraw of it ([`box_key`]).
struct BoxAsk {
    key: String,
    /// The escalation was the memory measure's: the point's own act was
    /// replaced by the person's restart of an agent the machine's trouble was
    /// acted on for ([`Session::mark_memory_badge`]). A measure no longer acted
    /// on hands the point back and takes the badge down ([`Session::memory_edge`]).
    memory: bool,
}

/// What a wait for the turn's end may do about a rating card on its BUSY reads: the
/// loop's options and the survey state the
/// look keeps ([`Looking::survey`]) — lent to the wait so a card that appears and is
/// pressed during a long busy spell is judged by the same machine, on the next read.
/// Lent only under `dismiss_surveys`, and never by a loop whose Autopilot is Off.
struct SurveyDuty<'a> {
    opts: &'a SuperviseOpts,
    survey: &'a mut Survey,
}

/// What one step of the survey machine reads besides the turn: the card as the
/// reader found it, and whether the read is a BUSY one ([`SurveyDuty`]).
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

/// The pauses before each try of the survey's `0` after one that did not
/// dismiss it (the last for every try after), and from which failed try on
/// the host is told (no tab is marked) — while the tries go on: the turn-end
/// policy acts at no point with a survey open, so a survey handed over for
/// good would stop the session being continued, silently.
const SURVEY_RETRY: [Duration; SURVEY_TRIES] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];
const SURVEY_BADGE_AT: u32 = 2;
const SURVEY_TRIES: usize = 4;
/// How long a rating card must have shown before its `0` is pressed. Claude Code
/// drops a digit typed within `wm = 600` ms of the card mounting and leaves it in the
/// composer as text (the old first press landed 508-514 ms after the card, every
/// time, on a stand-in that applies the vendor's rule); this is that plus margin,
/// counted from the read that FIRST showed the card ([`Session::survey_young`]).
const SURVEY_SETTLE: Duration = Duration::from_millis(800);
/// How long a `0` the harness wrote may still be WAITING on Claude Code: the card takes
/// a digit that stands alone in the composer `debounceMs = 400` ms after it appeared, and
/// ANY change to the composer in that time cancels the pending take — a backspace of
/// ours included (the vendor's `zu` hook, 2.1.284). So the press is not judged, and its
/// `0` is not removed, until it has stood this long under a card still open: 5x the
/// debounce, so a timer that fires late on a loaded event loop still lands inside it. A
/// `0` that stands under an open card past it was dropped and is backspaced; before it,
/// the loop waits for the card to leave ([`Session::survey_pending`]).
const SURVEY_LAND: Duration = Duration::from_secs(2);

/// THE ACCOUNTS PROBE'S PANEL, as the loop remembers it
/// (`docs/SPEC-claude-accounts-probe-2026-10-01.md` §2, §3.1; aterm-spec
/// `supervisor_usage_probe_model`'s `ph`): whether a Settings panel THE
/// HARNESS opened may be on the session's screen. Everything but `Idle` is a
/// panel owed its close, and while one is owed no second `/usage` is typed
/// and no decision is made on a panel screen ([`Session::usage_panel_look`]).
/// It is in the session ledger too — a `typed` row with no `waiting` row
/// after it ([`approvals::probe_owed`]) — so the loop that follows this one
/// takes the panel up; with no such row nothing is ever closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageProbe {
    /// No panel of the harness's is owed.
    Idle,
    /// A panel of the harness's may be open: this loop wrote `/usage`
    /// (`own`) and has not seen the session without it, or the ledger says
    /// an earlier loop did. `since` is when this loop took it up: a panel
    /// taken up from the ledger is closed only once it has stood one
    /// person's grace in this loop's sight (the spec's P19 — a successor
    /// reads no person stamp from before it started).
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
    /// decided again NOW, through [`Session::turn_end_now`], which asks the
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
    /// How long the panel is waited for after the Enter. The command is
    /// `immediate` (READ in the 2.1.287 bundle) and the panel was on the
    /// first read after the Enter in every measured run (55 ms at 20 reads a
    /// second, 2026-10-01); this is half the fenced write's echo wait
    /// ([`turn_end_loop::REACTION_WAIT`]), for a loaded machine.
    open: Duration,
    /// How long the panel is read for a settled reading: twice the vendor's
    /// 5 s fetch timeout (reader-reported, 2.1.286).
    settle: Duration,
    /// The longest pause between two reads of the open panel
    /// ([`Session::probe_pace`]: the screen holding still 50 ms ends it
    /// sooner). NOT the loop's own settle (`await idle 500`, capped 1.5 s,
    /// which the spec named). MEASURED on 2.1.287, 2026-10-01: the whole
    /// tab is on an ordinary grid only while its "contributing" block is
    /// still scanning — 0.75 s at 60x50, 1.1 s at 129x47, 1.2 to 2.3 s at
    /// 150x50 — and under the fold for good after it, so a read that waits
    /// half a second of stillness first can read only the fold. A quarter
    /// second puts two reads inside the narrowest of those.
    step: Duration,
    /// How long the panel is given to leave after its Escape (`await gone`
    /// on its header row): the survey's landing figure ([`SURVEY_LAND`]).
    /// MEASURED: gone within 13 to 35 ms, eight closes of eight.
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

/// A LIMIT RESET'S STEP, as the loop remembers it
/// (`docs/SPEC-claude-accounts-resets-2026-10-02.md` §2, §3.1; aterm-spec
/// `supervisor_limit_reset_model`'s `ph` and `own`): whether THE HARNESS took
/// the usage-limit dialog's grant row and has not yet told its host how that
/// ended. While one is owed it is dealt with first at every look
/// ([`Session::reset_look`]), no second row is taken, and no rule of the
/// approval policy sees the pending line or the confirmation the row opened.
/// It is in the session ledger too — the rule's `approved` row with no
/// `waiting` row after it ([`approvals::reset_owed`]) — so the loop that
/// follows this one takes the step up; with no such row a confirmation on the
/// screen is a person's, and is not touched here.
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
    /// The host's go for the row THIS loop took and saw taken: what the
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
    /// before it has stood [`Session::box_settle`] from there (the vendor
    /// refuses a key sooner, and re-arms on it), and a step taken up waits
    /// one person's grace from there.
    seen: Option<Instant>,
    /// The count and the placed deadline of a confirmation that read
    /// exactly: the offer the host records.
    offer: Option<(u32, Option<i64>)>,
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

/// Where `--context-warn`'s watch on the context indicator stands
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

/// An episode closed at a busy read ([`Session::limit_on_busy`]) or by an
/// Autopilot Off park ([`Session::lower_badges`]), kept until the worker
/// answers on a point that is not the notice ([`Session::look`], or a read
/// of the park, [`Session::park_while_off`]). The wall
/// again before that is the retry hitting it (or the notice the park left
/// up): the same episode reopened ([`Session::open_episode`]), attention set
/// again, no second mail.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Retry {
    /// [`Episode::seq`] of the episode that closed.
    seq: u64,
}

/// What [`Session::auto_read`] made of a turn.
enum Step {
    /// A box was answered (approved, or declined with a reason), or the box
    /// had left: look again. `settle` makes
    /// the first wait the settle wait, not `await gone` (the busy footer is not
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

/// The half of the loop that differs between `supervise` and `watch`.
trait Review {
    /// A read-only prompt was approved; the press landed at `seq`.
    fn approved(&mut self, seq: u64, command: &str) -> Result<(), String>;
    /// A new review point, with its report counted when `--report` asked for
    /// one, and how `--mail` settled it when it was an idle one. `false`
    /// stops the loop with it (`supervise`); `true` means it was reported and
    /// the loop keeps watching (`watch`).
    fn review(
        &mut self,
        turn: &Turn,
        report: Option<ReportBrief>,
        fold: Option<Fold>,
    ) -> Result<bool, String>;
    /// An informational line (`RECONNECT …`, `RECONNECTED …`), said as it
    /// happens.
    fn say(&mut self, line: &str) -> Result<(), String>;
    /// A line for the journal alone (`ESCALATED …`, `CLEARED …`, `WAITING …`):
    /// recorded, never printed, so the lines a harness reads are the same
    /// with and without a limit.
    fn note(&mut self, line: &str);
    /// Whether this half runs unattended (`watch`): a limit is then the
    /// loop's to handle ([`Session::turn_end_at_point`]). `supervise` stops at
    /// the limited point with the manager right there, and does none of it.
    fn unattended(&self) -> bool;
}

/// `await-turn` ([`Session::await_turn`]): no loop around the wait — nothing
/// is said, noted or handled; a limit is the caller's.
struct Alone;

impl Review for Alone {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(
        &mut self,
        _turn: &Turn,
        _report: Option<ReportBrief>,
        _fold: Option<Fold>,
    ) -> Result<bool, String> {
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

/// `supervise`: the first review point ends the loop, and is its result; an
/// informational line goes to the sink (stderr), never into the result. The
/// journal gets every line said, and the line `watch` would print for an
/// approval and for the review point.
struct StopAtReview<'l, 'w> {
    sink: &'l Mutex<Sink<'w>>,
    allow: &'l [String],
}

impl Review for StopAtReview<'_, '_> {
    fn approved(&mut self, seq: u64, command: &str) -> Result<(), String> {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(&approved_line(seq, command), None);
        Ok(())
    }
    fn review(
        &mut self,
        turn: &Turn,
        _report: Option<ReportBrief>,
        fold: Option<Fold>,
    ) -> Result<bool, String> {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(&folded_event_line(turn, self.allow, None, fold), None);
        Ok(false)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        sink.record(line, None);
        log_line(sink.out, line);
        Ok(())
    }
    fn note(&mut self, line: &str) {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(line, None);
    }
    fn unattended(&self) -> bool {
        false
    }
}

/// `watch`: one flushed line per approval and per review point, each
/// journaled before it is printed.
struct Lines<'w, 's> {
    sink: &'w Mutex<Sink<'s>>,
    allow: &'w [String],
}

impl Review for Lines<'_, '_> {
    fn approved(&mut self, seq: u64, command: &str) -> Result<(), String> {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .line(&approved_line(seq, command), None)
    }
    fn review(
        &mut self,
        turn: &Turn,
        report: Option<ReportBrief>,
        fold: Option<Fold>,
    ) -> Result<bool, String> {
        let line = folded_event_line(turn, self.allow, report, fold);
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .line(&line, report.and_then(|b| b.turn))?;
        Ok(true)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .line(line, None)
    }
    fn note(&mut self, line: &str) {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(line, None);
    }
    fn unattended(&self) -> bool {
        true
    }
}

/// The EVENT line of a review point, as `--report` and `--mail` shape it:
/// [`reported_event_line`]'s with a brief, [`event_line`]'s without. An idle
/// point `--mail` folded a report into is `EVENT turn seq=<n> report=<id>
/// rows=<n> <summary>` (no brief, which would repeat the report), and one no
/// report came for is `EVENT idle-no-report …` with the brief if any.
fn folded_event_line(
    turn: &Turn,
    allow: &[String],
    report: Option<ReportBrief>,
    fold: Option<Fold>,
) -> String {
    match fold {
        Some(Fold::Report { id, rows }) => format!(
            "EVENT turn seq={} report={id} rows={rows} {}",
            turn.screen.seq,
            event_summary(turn, allow)
        ),
        Some(Fold::NoReport) => event_line_as("idle-no-report", turn, allow, report),
        None => event_line_as(turn.phase.name(), turn, allow, report),
    }
}

/// The line `watch` prints for an approval: `APPROVED seq=<n> <command>`.
fn approved_line(seq: u64, command: &str) -> String {
    format!("APPROVED seq={seq} {}", clip(command))
}

/// What one guarded press did. `seq` is the content baseline the server
/// stamped on its answer (the screen read before it, when it stamped none):
/// the caller waits for the screen to move PAST it before looking again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Press {
    /// The answering key was written while the box was up: the `1`, a
    /// dialog's Enter, or a decline's Enter on its text.
    Pressed { seq: u64 },
    /// Nothing was written: the box was no longer on the screen at the check
    /// (or, on the fallback path, the digit landed in the composer and was
    /// backspaced — the prompt had resolved on its own).
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
    /// Nothing was written: the host has no guarded press, and the session
    /// survey is open — an unguarded `1` that lands after the box resolved
    /// is a rating (`1: Bad`), and leaves no stray digit to find.
    Withheld,
    /// The connection was lost first. `known` is what the loop knows the press
    /// did — `None` when the press itself got no answer, so the `1` may or may
    /// not have been written — and `why` the failure.
    Lost { known: Option<Press>, why: String },
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

/// Whether a digit press may go without the screen-generation fence
/// ([`Session::press_one_guarded`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fence {
    /// Fenced where the host has the fence; a host that turns the fenced key
    /// away is pressed under the row guard alone, and one without `key if=`
    /// by the confirmed fallback: the window's own press of a box's digit.
    WhereKnown,
    /// Fenced, or not pressed at all: `aterm drive answer`'s digit on a
    /// question read by its answers' roles (Codex's), whose guard names only
    /// the title row every one-question dialog draws the same.
    Required,
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
    /// Nothing was written: a PERSON gave the session input since the read
    /// (`OK skipped reason=person`, the `if-human=` fence). No try was spent;
    /// the next look reads again, and their grace holds the card.
    Yielded,
    /// The host has no `key if=`: nothing is pressed unguarded, and the
    /// survey is said as `EVENT survey …` instead.
    Unguarded,
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

/// One driven session: the transport, the target selector, the probed caps,
/// how long an outage is ridden out, and what the loop carries across one.
pub struct Session<'a, C: Ctl> {
    ctl: &'a mut C,
    sid: Option<String>,
    caps: Caps,
    /// How long an outage is ridden out before the loop ends
    /// ([`Self::ride_out`]); zero ends the loop at its first unserved request.
    reconnect: Duration,
    /// The first pause between tries to reach the session again, and the
    /// longest (each doubles).
    pause: Duration,
    pause_max: Duration,
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
    /// A `1` the fallback press may have left in the composer, unchecked
    /// because the connection was lost first: the command it answered. The
    /// next turn's screen is checked for it ([`Self::look`]).
    stray: Option<String>,
    /// A screen read since the loop last looked at the session survey showed
    /// none open ([`Self::screen`], [`Self::survey`]) — a busy read on the
    /// way to a turn included, so a survey that leaves and comes back while
    /// the worker is busy is a new appearance.
    survey_gone: bool,
    /// `--context-warn`'s threshold for the loop in progress
    /// ([`Self::watch_context`]), set by [`Self::drive`] from its options:
    /// `0` — off — until then, so `await-turn` says nothing about it.
    context_warn: u8,
    /// Where the watch on the context indicator stands.
    context: Context,
    /// `--mail`'s side of the lane while a loop runs with one.
    mail: Option<MailIn>,
    /// How long the lane parks one `await inbox` before re-arming it.
    mail_step: Duration,
    /// How long a held idle point waits between looks at the screen.
    hold_step: Duration,
    /// The manager's session, `@sid`, for the escalation mail
    /// ([`Self::set_manager`]); `None` posts none.
    manager: Option<String>,
    /// The limit episode in progress, if any.
    limit: Option<Episode>,
    /// The episode a busy read or an Off park closed, until the worker
    /// answers ([`Retry`]): the notice again before that opens it again.
    retry: Option<Retry>,
    /// The box escalated to the manager, until it leaves the screen
    /// ([`Self::escalate_point`]).
    box_ask: Option<BoxAsk>,
    /// A fixed clock, Unix seconds (tests); `None` reads the system's.
    clock: Option<i64>,
    /// Tests: how far the fixed clock jumps ahead after its next read, so a
    /// Mac's sleep can be staged between two reads ([`Self::set_clock_jump`]);
    /// `clock_skew` is the jump once that read is made.
    #[cfg(test)]
    clock_jump: std::sync::atomic::AtomicI64,
    #[cfg(test)]
    clock_skew: std::sync::atomic::AtomicI64,
    /// Tests: the instant the fixed clock starts RUNNING from, so it keeps
    /// pace with the monotonic clock ([`Self::set_clock_running`]); `None`: it
    /// stands still.
    #[cfg(test)]
    clock_runs_from: Option<Instant>,
    /// Tests: the fixed clock's milliseconds past its whole second
    /// ([`Self::set_clock_at_ms`]); zero for [`Self::set_clock`].
    #[cfg(test)]
    clock_phase_ms: i64,
    /// Tests: how long a hold for a person waits before the point is looked
    /// at again, in place of the rest of their grace
    /// ([`Self::set_person_hold_wait`]); `None`: the rest of their grace.
    #[cfg(test)]
    person_hold_wait: Option<Duration>,
    /// The local zone's offset, read once from `date` when first needed.
    local_offset: Option<i64>,
    /// A zone's offset by name at an instant ([`limit::zone_offset_at`]; a
    /// table in tests).
    zone_offset: fn(&str, i64) -> Option<i64>,
    /// Whether a Codex (by pid) still LEADS ITS TERMINAL — alive, its group
    /// its terminal's foreground: the kernel's word
    /// ([`crate::harness::upgrade_drive::leads_its_terminal`]; a stand-in in
    /// tests). The paused-goal box of the Codex the live upgrade relaunched
    /// is the only one its approval policy answers for it
    /// ([`Self::goal_box_is_upgrades`]).
    pub(super) codex_leads: fn(u32) -> bool,
    /// What the turn-end policy remembers of this session
    /// ([`super::policy::turn_end::TurnEndState`]).
    turn_end: TurnEndState,
    /// When the turn-end policy decides again at the point still showing
    /// (its `WaitUntil`), and why — the deadline of the loop's wait for the
    /// screen to move ([`Self::wait_for_next`]), on both clocks ([`Due`]).
    turn_end_due: Option<Due>,
    /// The two clocks as the loop last read them — the monotonic one, and the
    /// wall clock in unix milliseconds: the marks of its sleep watch
    /// ([`Self::watch_sleep`]). The SESSION's, read after every request and
    /// at each step of a wait, whatever point shows: a repaint at the wake (a
    /// new seq) or an edge that decides first sees the sleep all the same.
    clock_marks: Option<(Instant, i64)>,
    /// A sleep the watch measured whose wake [`Self::woke`] has not yet
    /// looked at for a due the sleep carried past ([`Self::check_slept`]): the
    /// wall clock when it was measured, unix seconds.
    slept_at: Option<i64>,
    /// THE WAKE'S SETTLE, placed by the watch where it measured the sleep
    /// ([`Self::settle_wake`]): until it falls no turn is typed into the agent
    /// — no act of the turn-end policy's ([`Self::turn_end_execute`]), no step
    /// of the host's ([`Guard::WakeSettle`]), no accounts probe
    /// ([`ProbeGate::WakeSettle`]) — and a point the policy acted on is
    /// decided again then; and whether its `WAITING … the Mac woke` has been
    /// said.
    wake_settle: Option<(Due, bool)>,
    /// A restart the host could not make yet, in a row
    /// ([`Self::restart_agent`]): each next try waits longer.
    restart_waits: u32,
    /// When the loop looks again at the point still showing, whatever the
    /// turn-end policy says: a try of the survey's `0` that is owed
    /// ([`Survey::Retry`]). Ends the loop's wait ([`Self::wait_for_next`]).
    look_at: Option<Instant>,
    /// The survey's retry pauses ([`SURVEY_RETRY`]; a test shortens them).
    survey_retry: [Duration; SURVEY_TRIES],
    /// When a read FIRST showed the rating card now open (`None`: none is, or none
    /// has been seen since the last read that showed it gone) and how long it must
    /// have shown before its `0` goes ([`SURVEY_SETTLE`]).
    survey_first_seen: Option<Instant>,
    survey_settle: Duration,
    /// When the harness's `0` for a rating card went in, until it is accounted for — the
    /// card took it, a backspace removed it, or a read past [`SURVEY_LAND`] shows no
    /// lone `0` in the composer — and how long the card is given to take it
    /// ([`SURVEY_LAND`]). The press is judged, and a lone `0` removed, by this and not by
    /// the survey machine's state, so a `0` is never forgotten in one of its states.
    survey_zero: Option<Instant>,
    survey_land: Duration,
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
    /// first busy read since the last review point, a break the switch took
    /// as its point, or the rate-limit nudge's box — the turn's work, as the
    /// loop saw it, [`TurnEndReading::worked`]) and a person's hand latched
    /// in it ([`Self::person_in_this_turn`]).
    running: super::policy::turn_end::RunningTurn,
    /// A wall's reset as read, by its notice's text: the same text is the
    /// same reset (a span counts from the print, not from every read).
    wall_reset: Option<(String, Option<i64>)>,
    /// The stuck-busy canary ([`wall_memory`]): when it last asked the server
    /// what the agent's own title says, and how long a screen must read busy
    /// on background work alone before it does.
    stuck_asked: Option<Instant>,
    /// How far up the canary's ladder this stall has climbed
    /// ([`wall_memory::StuckRung`]), and since when its third rung stood, of
    /// which kind (the end's `CLEARED` line names it).
    stuck_rung: wall_memory::StuckRung,
    stuck_raised: Option<(Instant, super::policy::outlook::ExceptionKind)>,
    stuck_after: Duration,
    /// The canary's second flavour ([`Session::dialog_note`]): the words of the
    /// dialog no reader names that has stood on the screen, and when it was
    /// first seen. The ladder's other fields are shared with the busy flavour,
    /// which never stands together with it.
    stuck_dialog: Option<(String, Instant)>,
    /// How many reprints of the same words had a far jump of the reset held
    /// back ([`turn_end_loop::REPRINT_JUMPS_MAX`]).
    reprint_jumps: u32,
    /// The act of the loop's a wall was last read to have ANSWERED (its
    /// instant, [`TurnEndState::untaken_act`]), and the wall's words at that
    /// read (`turn_end_loop::reset_key`): that read placed the wall's reset on
    /// the print, once an act and its words.
    wall_answered: Option<(Instant, String)>,
    /// The wall the loop waits on, kept while the screen
    /// shows only the agent's own background work ([`wall_memory`]).
    wall_memory: Option<wall_memory::WallMemory>,
    /// The verdict the last decision's read gave the screen's wall
    /// ([`wall_confirm`]: confirmed or refused, by the screen it was read
    /// at), and the refusal last journaled.
    wall_verdict: Option<wall_confirm::Verdict>,
    wall_refused_said: Option<String>,
    /// The LIMIT A CANCEL LEFT that the last decision's read took for the
    /// answer to a turn ([`TurnEndState::left_answer`]), by the screen it was
    /// read at, and whether a transcript row dated it: what that screen's
    /// reading and its point read as ([`Self::point_as_decided`]) for the
    /// loop's books after the decision, and where its reset is placed
    /// ([`Self::wall_reset_at`]).
    left_read: Option<wall_confirm::LeftRead>,
    /// The id (`uuid`) of the transcript row that confirmed the screen's
    /// usage limit at the last decision's read of a wall: the limit's own
    /// notice, which a reply must follow for the transcript to say a turn was
    /// served since it ([`wall_confirm`]'s `served_since_notice`). `None`
    /// where the last wall read was no limit, or was not confirmed by a
    /// transcript (none to read, or refused).
    limit_notice_row: Option<String>,
    /// What the approval policy takes from the supervisor's own process
    /// (the owner's home, uid and `$TMPDIR`; fixed in tests).
    approval_env: ApprovalEnv,
    /// The session's working directory as its `meta` last said it (`None`
    /// for `cwd=-` or unread; read fresh for every box that needs it).
    cwd: Option<PathBuf>,
    /// The permission mode the worker's footer last showed: a box replaces the
    /// footer, so the box's own read cannot say whether the session bypasses
    /// permissions.
    footer: Option<crate::harness::lights::Mode>,
    /// A busy frame of this session was read. Its footer names any mode but
    /// the default, so after one a mode still unread ([`Self::footer`]) is
    /// no race of the agent's start, and the verdict wait is back
    /// ([`Self::agent_wake`]). Read only while no footer has been.
    busy_read: bool,
    /// Where the approval ledger goes ([`Self::set_approval_ledger`]);
    /// `None` keeps none.
    ledger_path: Option<PathBuf>,
    /// The ledger of the loop in progress.
    ledger: Ledger,
    /// The turn-end policy's typing budget was seeded from the ledger
    /// ([`TurnEndState::seed_acts`]): once per session, at its first run.
    turn_end_seeded: bool,
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
    /// The last `status` named another driver's hand on the session (a drive
    /// lease, a named driver's turn): it holds the loop as a person does.
    driver: bool,
    /// How long presses that miss are tried before the session is badged
    /// ([`approval_loop::PRESS_BADGE_AFTER`]; a test shortens it).
    press_badge_after: Duration,
    /// What the loop remembers of the question dialog it is answering
    /// ([`approval_loop::QuestionMemory`]): its first-key wait, its last key
    /// not yet seen taking effect. Cleared when the worker works again.
    question: approval_loop::QuestionMemory,
    /// What the loop remembers of the decline it is carrying out
    /// ([`approval_loop::DeclineMemory`]): its last keystroke not yet seen
    /// drawn, the re-raised box's pause. Cleared when the worker works again.
    decline: approval_loop::DeclineMemory,
    /// The question dialog last decided was handed to the person BY THE
    /// POLICY — a word that can change while it waits — so the wait for it
    /// re-reads the session's word ([`Self::wait_for_next`]).
    question_held: bool,
    /// The policy that held the dialog no longer does: the next look at it is
    /// a fresh decision, not the point already handed over.
    question_handed_back: bool,
    /// This loop tells the window its own story lines on its own connection
    /// (`story chose …`): the hosted loop, which has no teller of its own
    /// ([`Self::run_hosted`]). A `watch` with a teller tells through it.
    tell_own: bool,
    /// The policy text's exact read-back ([`Self::press_question_text`])
    /// and how long the screen had held still right before it (its settle
    /// latched): the read the Enter on it is decided on
    /// ([`Step::Decide`]). Taken by the press's caller at once.
    read_back: Option<(Screen, Duration)>,
    /// The back-off after `ERR busy …`/`ERR rate`: doubles to
    /// [`REFUSAL_PAUSE_MAX`], reset by a press the server takes.
    refusal_pause: Duration,
    /// The refusal the journal last said, by its episode's key (`REFUSED
    /// <why>`, `HALTED <why>`, its measures dropped), and when it was last
    /// met: one that persists is retried and said once, again when it
    /// changes ([`Self::refusal_news`]). Over at the first write the server
    /// does not refuse ([`Self::call`]), at a hold seen lifted, and once it
    /// has not been met for [`REFUSAL_QUIET`].
    refusal_said: Option<(String, Instant)>,
    /// Set by the hosted loop's owner: the loop ends at its next look.
    stop: Option<Arc<AtomicBool>>,
    /// Set by the hosted loop's owner, BEFORE [`Self::stop`], when the stop
    /// hands the session to a loop it starts right after (a changed
    /// policy): the badges this loop raised are left standing for that one
    /// to adopt ([`Self::reconcile`]), never cleared and raised again.
    handover: Option<Arc<AtomicBool>>,
    /// A `limited:` attention a previous watcher left over the notice still
    /// showing, adopted at the start: the episode it opens posts no mail —
    /// after an Off park that took the badge down too
    /// ([`Session::lower_badges`] keeps the marker). Over once the worker
    /// answers on a point that is not the notice, at a look or a read of a
    /// park ([`Session::adopted_limit_over`]): a limit after that is mailed.
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
    /// When a PERSON last typed into the session, as that status read said
    /// (`human_ms=`; `None`: none, or a server that does not say).
    person: Option<Instant>,
    /// The draft in the composer as the last read showed it, and when a read
    /// first showed it so ([`Self::screen`]): a draft that changes is a
    /// person typing, whatever the server says ([`TurnEndReading::person`]).
    draft_seen: Option<(String, Instant)>,
    /// This look held an act for a person at the keyboard ([`Self::
    /// held_for_person`]: a box's answer, the survey's `0`, a stray digit's
    /// backspace): nothing escalated, the point looked at again at
    /// [`Self::turn_end_due`].
    held: bool,
    /// The deadline of a held act ran out: the next look judges the point
    /// on the screen again, as a new one.
    rejudge: bool,
    /// The point showing was left to the host: decided while it owned turn
    /// ends ([`IdleHost::owns_turn_end`]), or passed over for a step that typed
    /// and ended nothing ([`Self::host_steps_here`], [`HostStep::moved`]). Once
    /// the host owns nothing the point is decided again
    /// ([`Self::wait_for_next`]), so it never sits. A point a step moved is
    /// not held: it is gone.
    host_held: bool,
    /// The box on the screen (its review key) and when a read first showed
    /// it: judged only once it has shown [`Self::box_settle`].
    box_first_seen: Option<(String, Instant)>,
    /// [`BOX_SETTLE`] ([`Self::set_box_settle`]).
    box_settle: Duration,
    /// [`background_settle`] ([`Self::set_background_settle`]).
    background_settle: Duration,
    /// The point (its review key) an `await agent` last woke the loop from
    /// ([`Self::agent_wake`]): the same point read again means the server's
    /// verdict and the loop's reading disagree there, and it is waited on by
    /// its content instead.
    agent_woke: Option<String>,
    /// Where the loop stands on the session's supervisor claim
    /// ([`claim::Claim`]).
    claim: claim::Claim,
    /// What the claim's renewals changed, for the next look to say.
    claim_said: Vec<String>,
    /// The claim's renewal step and lease ([`claim::CLAIM_RENEW`],
    /// [`claim::CLAIM_TTL_MS`]; tests shorten them).
    claim_renew: Duration,
    claim_ttl_ms: u64,
    /// The holder the claim is taken under ([`Self::set_supervisor_name`]).
    supervisor_name: Option<String>,
    /// A PROVEN SELF-CLAIM's holder this loop watches beside
    /// ([`Self::set_beside`], [`super::claim::Claim::Beside`]): it claims
    /// nothing and types nothing, and keeps the stall's remedy.
    beside: Option<String>,
    /// The text of this loop's own keyed attention entry (owner
    /// [`ATTENTION_OWNER`]), as last set and not yet unset.
    attention_ours: Option<String>,
    /// The turn-end policy raised a badge that no point carries (`/login`
    /// typed, "finish sign-in in the browser"): cleared once the worker
    /// works again.
    turn_end_badge: bool,
    /// That badge's text, taken down as an Off park started
    /// ([`Session::lower_badges`]) and set again as the park ends on a word
    /// ([`Session::raise_parked_badge`]); forgotten where a read of the
    /// park sees the worker working, which is that badge's end.
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
    /// The host's measure of the API's reach at the point on screen when that
    /// point is a network wall (never reached the API, cut off, refused a
    /// certificate or proxy; [`Self::turn_end_reading`]); `None` otherwise.
    /// While `Some` and a policy wait stands, each wait step asks the host
    /// again, and a new measure decides the point again ([`Self::reach_edge`]).
    reach_seen: Option<Reach>,
    /// What the host advised about the session's account at the point being
    /// decided ([`IdleHost::account`]), with the screen and the point it was
    /// asked for: the answer for every reading of that point within ONE
    /// decision ([`Self::turn_end_reading`] builds several). Forgotten where
    /// a decision begins ([`Self::account_round`]), so a point decided again
    /// asks again.
    account_seen: Option<(u64, AccountPoint, Option<AccountAdvice>)>,
    /// The day count of Claude Code's expiry notice this loop last told its
    /// host ([`IdleHost::login_notice`]): each value is told once.
    login_notice_told: Option<u32>,
    /// The host's stamp ([`IdleHost::account_stamp`]) as this session last
    /// read it — at a decision, before the host is asked
    /// ([`Self::account_advise`]), or at a step of a wait: one that has
    /// moved since is the accounts' news ([`Self::account_news`]), and
    /// decides a standing wait again ([`Self::account_edge`]). `None` until
    /// the first read, which is no news: the stamp counts since the HOST
    /// started, and a loop that starts later has read none of it.
    account_stamp: Option<u64>,
    /// When the accounts' news last ended a LATCHED session's wait
    /// ([`Self::account_edge`]): the next such news is heard no sooner than
    /// [`ACCOUNT_NEWS_EVERY`] after it.
    account_news_at: Option<Instant>,
    /// The host's stamp ([`IdleHost::account_stamp`]) as read just before
    /// the host said the process at the lost login on screen holds its
    /// renewable row's OWN login ([`IdleHost::login_superseded`] `false`, at
    /// a decision's read, [`Self::turn_end_reading`]): the point decided on
    /// that reading asks the owner for the renewal. `None` at every other
    /// decision's read. A step of a wait whose stamp has moved past it asks
    /// the host again, and a renewal made since decides the point again
    /// ([`Self::renewal_made`]).
    login_seen: Option<u64>,
    /// The accounts probe's panel, as this loop remembers it
    /// ([`UsageProbe`]): `Idle` for the whole life of every loop on a
    /// machine with no roster, where no probe is claimed and the session
    /// ledger holds no row of one.
    probe: UsageProbe,
    /// The grid (`cols`, `pane_rows`) at which a probe never read the
    /// panel's tail — every read of it was the fold, from the first to the
    /// last ([`ProbeMiss::Fold`]): that size is not probed again by this loop
    /// (the spec's P11: a short window would be typed into every three
    /// minutes for nothing). A resize is another size.
    probe_fold: Option<(Option<u16>, Option<usize>)>,
    /// This loop probes no more: what came up was not the Usage tab under
    /// the fullscreen renderer's rule, or the point's key changed across a
    /// probe (the spec's P21) — something the step left on the screen.
    probe_off: bool,
    /// The look in progress took its point after a read of the worker's OWN
    /// busy row ([`Saw::live`]): the turn that just ended was this
    /// process's, so its last response is newer than any usage snapshot the
    /// home holds (the spec's P7). Set where the look takes what the reads
    /// saw; `false` at every other look, a point decided again included.
    turn_live: bool,
    /// The point ([`Session::review_key`]) the loop last SPOKE FOR about its
    /// probe: where it said which gate shut on a due one
    /// ([`Session::probe_shut`]), or where a probe was claimed. The same
    /// point decided again — an idle session's wait running out, hour after
    /// hour — says nothing more: one line a point. Forgotten where a look
    /// takes its point after a busy read ([`Session::look`]): a turn ran, and
    /// the point after it is another, whatever it says.
    probe_said: Option<String>,
    /// Until when the owed panel's Escape is left to a person's hand
    /// ([`Session::usage_panel_look`]): the `HELD` line is said once a hold.
    probe_hold: Option<Instant>,
    /// The probe's bounds ([`PROBE_WAITS`]; a test shortens them).
    probe_waits: ProbeWaits,
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
    /// The dialog mount (its [`Self::box_first_seen`] key) the reset was
    /// passed over for — a gate of the loop's shut, the host said no, the
    /// row's press never landed: the rest of that mount is the wait row's,
    /// and its host is not asked again. Forgotten with the mount
    /// ([`Session::box_young`]).
    reset_passed: Option<String>,
    /// How many times in a row the host answered that it cannot say yet
    /// ([`ResetWord::Again`]) at this mount ([`reset_loop::RESET_AGAIN_MAX`]).
    reset_again: u8,
    /// What the usage-limit dialog is answered with where its reset is
    /// passed over: the decision made without the reset's rule, on the same
    /// read ([`Session::decide_box`]) — the one a build without resets, or
    /// with the key off, makes.
    reset_fallback: Option<Decision>,
    /// The reset step's bounds ([`reset_loop::RESET_WAITS`]; a test shortens
    /// them).
    reset_waits: reset_loop::ResetWaits,
    /// Whether the last focus press ([`Session::press_focused`]) reached its
    /// answering key: a press lost before it wrote arrows at most, which
    /// choose nothing.
    focus_key_tried: bool,
    /// `status` last said the session is halted (`hold=1`,
    /// [`Session::foreground_program`]).
    halted: bool,
    /// The session's `autopilot` word as the last bare `meta` named it
    /// ([`approval_loop::AutopilotWord`]): the turn-end policy's mode, and at
    /// `off` the loop's park ([`Session::park_while_off`]).
    autopilot: approval_loop::AutopilotWord,
    /// The local zone's offset at an instant ([`limit::offset_at`] with no
    /// zone; a table in tests): what a day the vendor draws with no zone is
    /// placed by ([`Session::reset_place`]).
    local_offset_at: fn(i64) -> Option<i64>,
    /// The host's measure of the agent's memory past `[harness]
    /// memory_ceiling` AND ACTED ON ([`super::memory::MemoryOver::acts`]:
    /// the Mac in trouble, this agent the largest) as the last turn-end
    /// reading judged it ([`Self::turn_end_reading`]): `None` otherwise — a
    /// warning only, under the ceiling, no host or no measure. A wait at an
    /// idle point decided without one asks the host again at the top of each
    /// step, and a measure acted on decides the point again at once
    /// ([`Self::memory_edge`]).
    memory_seen: Option<super::memory::MemoryOver>,
    /// The last turn-end reading took [`Self::memory_seen`] as its wall
    /// ([`Self::turn_end_reading`]: an idle point, a wall's own included):
    /// an escalation decided there is the memory's, raised by the point even
    /// at a limit notice ([`Self::escalates_here`]).
    memory_wall: bool,
    /// The last turn-end reading took the memory wall in place of a screen wall
    /// (a usage or spend limit, a lost login, an API error waited out), whose
    /// own act was replaced by the memory's restart or escalation. A measure
    /// back under the ceiling is then an edge too ([`Self::memory_edge`]): the
    /// point is decided again by the wall's own rule.
    memory_over_wall: bool,
    /// The memory warning was said ([`Self::memory_warn`]) with this verdict:
    /// once an episode (which ends a measure an eighth under the ceiling,
    /// [`super::memory::episode_over`]), and again whenever the verdict changes.
    memory_told: Option<super::memory::Verdict>,
    /// The host's last word on a memory restart was a REFUSAL
    /// (`restart_agent`'s `otherwise`: the escalation `the restart could not
    /// be made`), so the warning's verdict is the person's restart
    /// ([`super::memory::Verdict::Person`]) as the badge says, never aterm's.
    /// Cleared ONLY by a restart made
    /// (`restart_agent`'s `adopted`, any restart's; a memory restart's names
    /// aterm's at once, [`Self::memory_say`]): a refusal is mostly the
    /// agent's own launch (`refused:one-shot`, `refused:self`), which stands
    /// for its life, and only an idle point that asks the host again learns
    /// otherwise — a `wait:` leaves it, a refusal sets it again. Not on the
    /// agent working: the row then promised aterm's restart for every turn
    /// and took it back at every point. Not on a measure no longer acted on,
    /// nor at the warning episode's end: at Claude Code's own banner nothing
    /// hands the point back or asks again, and the row promised aterm's
    /// restart beside the badge until a person acted. Pessimistic between,
    /// never false.
    memory_refused: bool,
    /// The wait last told to the host ([`IdleHost::waiting`]): `Some(None)`
    /// is a told `None`, `None` is nothing told yet. The wait is told again
    /// only when its deadline moves, or when the unix second it was told as
    /// no longer agrees with the loop's clock ([`WAIT_SKEW_S`]).
    wait_told: Option<Option<ToldWait>>,
    /// A Codex session's model and effort as its footer last showed them —
    /// kept, since a box covers the footer ([`Self::note_codex_screen`]).
    codex_setting: Option<super::policy::turn_end::CodexSetting>,
    /// Codex's goal as the footer last showed it (`None`: none, or never read).
    codex_goal: Option<aterm_phase::codex::CodexGoal>,
    /// The last look at Codex's own records ([`Self::codex_records`]), and
    /// when it was taken.
    codex_seen: Option<(Instant, super::codex_usage::CodexSeen)>,
    /// A test's fixed reading of Codex's records, in place of the look.
    codex_fixed: Option<super::codex_usage::CodexSeen>,
    /// The sandbox fall this loop said (once per fall).
    sandbox_said: Option<String>,
    /// The save-then-wait switch's edges and notes not ledgered or said yet.
    wind_backlog: Vec<super::policy::turn_end::WindEvent>,
    /// The last busy read of the worker's own turn (a break under a Codex
    /// background terminal the switch took as its point is none): when the
    /// session last worked, as Codex's records are read around it
    /// ([`Self::codex_records`]).
    last_busy_at: Option<Instant>,
    /// The note that Codex's goal runs on after every stop is up
    /// ([`super::policy::turn_end::WindEvent::GoalNote`]): KEPT while the
    /// switch stands and the goal runs, never cleared by the goal's next
    /// turn — cleared once the switch has closed or the footer shows the
    /// goal stopped.
    goal_badge: bool,
    /// The break under a Codex background terminal the save-then-wait
    /// switch has taken as its point, while it lasts
    /// ([`Self::switch_at_background`]).
    switch_break: Option<turn_end_loop::SwitchBreak>,
    /// How long the switch may stand still through one such break before a
    /// person is told ([`turn_end_loop::SWITCH_BREAK_NOTE`]; a test shortens
    /// it).
    switch_break_note: Duration,
    /// `[harness] human_grace_s`, as the busy reads' goal stop reads it
    /// ([`Self::stop_codex_turn`]; set from the loop's policy at its start).
    human_grace: Duration,
    /// The stall `status input=stalled|stopped` reported and this loop holds
    /// on ([`stall`]): nothing is pressed, typed or escalated until it lifts.
    stalled: Option<stall::WorkerStall>,
    /// The host told of the stall held and lifted ([`IdleHost::stalled`]),
    /// and of what each read says of the account's usage
    /// ([`IdleHost::usage_seen`]): the loop's own, set as it starts.
    stall_host: Option<Arc<dyn IdleHost>>,
    /// What the host was last told of the account's usage, and when
    /// ([`Self::note_usage`]).
    usage_told: Option<(UsageSeen, Instant)>,
    /// The guard last said to the host ([`IdleHost::withheld`]), so each is
    /// said once while it stands: `None` once a point was offered, or while
    /// the host asked for none.
    withheld: Option<Guard>,
    /// A press was refused `ERR busy input-unread` or written with its
    /// `warn=input-unread` ([`CtlReply::warned_input_unread`]), or a `status`
    /// read for another reason reported a stall not held yet
    /// ([`Self::note_status`]): nothing is pressed, typed or escalated until
    /// `status` is read again ([`Self::stall_look`], [`Self::stall_step`]).
    stall_suspect: bool,
    /// This episode's stall has had its remedy, the explicit `restart` (or
    /// its warning)
    /// ([`Self::stall_remedy`]): never a second. Forgotten with the episode.
    stall_termed: bool,
    /// `[harness] human_grace_s`, for the remedy's own look at a person's
    /// hand: the loop's policy, set as it starts.
    stall_grace_s: u32,
    /// `[harness] stall_term_after_s`, the remedy's bound (`0`: never): the
    /// loop's policy, set as it starts.
    stall_term_after_s: u32,
    /// `[harness] stall_term`: the remedy at the bound is the explicit
    /// restart, `restart pid=<leader> if-stalled quiet=<grace>` (`true`; an
    /// older server that does not know the verb, `signal term`), or by
    /// default a warning with the agent left running ([`stall`]). Set as the
    /// loop starts.
    stall_term: bool,
}

/// The approval policy's inputs from the supervisor's own process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalEnv {
    pub home: Option<PathBuf>,
    pub uid: u32,
    pub tmpdir: Option<PathBuf>,
    /// Where the WORKER's environment comes from: the environment a git
    /// read's configuration is read with (`policy::git_config`), and whose
    /// `CLAUDE_CONFIG_DIR` (else `$HOME/.claude`) holds the session files and
    /// transcripts that say where its Bash tool stands
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
    pub fn new(ctl: &'a mut C, sid: Option<String>) -> Self {
        Self {
            ctl,
            sid,
            caps: Caps::default(),
            reconnect: DEFAULT_RECONNECT,
            pause: RECONNECT_PAUSE,
            pause_max: RECONNECT_PAUSE_MAX,
            outage: None,
            probing: false,
            last: None,
            busy_match: None,
            stray: None,
            survey_gone: false,
            context_warn: 0,
            context: Context::Armed,
            mail: None,
            mail_step: WAIT_STEP,
            hold_step: WAIT_STEP,
            manager: None,
            limit: None,
            retry: None,
            box_ask: None,
            clock: None,
            #[cfg(test)]
            clock_jump: std::sync::atomic::AtomicI64::new(0),
            #[cfg(test)]
            clock_skew: std::sync::atomic::AtomicI64::new(0),
            #[cfg(test)]
            clock_runs_from: None,
            #[cfg(test)]
            clock_phase_ms: 0,
            #[cfg(test)]
            person_hold_wait: None,
            local_offset: None,
            zone_offset: limit::zone_offset_at,
            codex_leads: crate::harness::upgrade_drive::leads_its_terminal,
            turn_end: TurnEndState::default(),
            turn_end_due: None,
            clock_marks: None,
            slept_at: None,
            wake_settle: None,
            restart_waits: 0,
            look_at: None,
            survey_retry: SURVEY_RETRY,
            survey_first_seen: None,
            survey_settle: SURVEY_SETTLE,
            survey_zero: None,
            survey_land: SURVEY_LAND,
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
            stuck_after: wall_memory::STUCK_AFTER,
            stuck_dialog: None,
            wall_memory: None,
            wall_verdict: None,
            wall_refused_said: None,
            left_read: None,
            limit_notice_row: None,
            approval_env: ApprovalEnv::of_process(),
            cwd: None,
            footer: None,
            busy_read: false,
            ledger_path: None,
            ledger: Ledger::off(),
            turn_end_seeded: false,
            box_reason: None,
            changed_streak: 0,
            press_retry: None,
            driver: false,
            press_badge_after: approval_loop::PRESS_BADGE_AFTER,
            question: approval_loop::QuestionMemory::default(),
            decline: approval_loop::DeclineMemory::default(),
            question_held: false,
            question_handed_back: false,
            tell_own: false,
            read_back: None,
            refusal_pause: REFUSAL_PAUSE,
            refusal_said: None,
            stop: None,
            handover: None,
            adopted_limit: false,
            adopted_box: false,
            program: None,
            status_sid: None,
            person: None,
            draft_seen: None,
            held: false,
            rejudge: false,
            host_held: false,
            agent_woke: None,
            box_first_seen: None,
            box_settle: BOX_SETTLE,
            background_settle: background_settle(),
            claim: claim::Claim::Off,
            claim_said: Vec::new(),
            claim_renew: claim::CLAIM_RENEW,
            claim_ttl_ms: claim::CLAIM_TTL_MS,
            supervisor_name: None,
            beside: None,
            attention_ours: None,
            turn_end_badge: false,
            badge_parked: None,
            exception_now: None,
            exception_open: None,
            task_done_said: false,
            reach_seen: None,
            account_seen: None,
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
            probe_waits: PROBE_WAITS,
            reset: ResetStep::Idle,
            reset_taken: None,
            reset_passed: None,
            reset_again: 0,
            reset_fallback: None,
            reset_waits: reset_loop::RESET_WAITS,
            focus_key_tried: false,
            halted: false,
            autopilot: approval_loop::AutopilotWord::default(),
            local_offset_at: |unix| limit::offset_at(None, unix),
            memory_seen: None,
            memory_refused: false,
            memory_wall: false,
            memory_over_wall: false,
            memory_told: None,
            wait_told: None,
            codex_setting: None,
            codex_goal: None,
            codex_seen: None,
            codex_fixed: None,
            sandbox_said: None,
            wind_backlog: Vec::new(),
            last_busy_at: None,
            goal_badge: false,
            switch_break: None,
            switch_break_note: turn_end_loop::SWITCH_BREAK_NOTE,
            human_grace: Duration::from_secs(120),
            stalled: None,
            stall_host: None,
            usage_told: None,
            withheld: None,
            stall_suspect: false,
            stall_termed: false,
            stall_grace_s: SupervisorConfig::default().human_grace_s,
            stall_term_after_s: SupervisorConfig::default().stall_term_after_s,
            stall_term: SupervisorConfig::default().stall_term,
        }
    }

    /// The holder an unattended loop claims the session under (`meta set
    /// supervisor <holder>`); unset, `aterm-supervise:<pid>`. The in-GUI
    /// host names itself so a CLI `watch` on the same session is second.
    pub fn set_supervisor_name(&mut self, name: Option<String>) {
        self.supervisor_name = name;
    }

    /// WATCH BESIDE A PROVEN SELF-CLAIM (the codex-alab self-claim contract,
    /// §4): the agent supervises itself under `holder`'s proven claim, so
    /// this loop takes no claim of its own and watches only — every write it
    /// would make refused locally, as behind any other holder — yet keeps
    /// aterm's remedies that act from outside the agent: the stall's
    /// explicit restart and the memory ceiling's measure
    /// ([`super::claim::Claim::Beside`]). `None`: claim as usual.
    pub fn set_beside(&mut self, holder: Option<String>) {
        self.beside = holder;
    }

    /// The hosted loop's hand-over flag ([`Self::handover`]): the host sets
    /// it before the stop when a new loop takes the session straight after.
    pub fn set_handover(&mut self, flag: Arc<AtomicBool>) {
        self.handover = Some(flag);
    }

    /// Tests: the claim's renewal step and lease.
    #[cfg(test)]
    pub(crate) fn set_claim_timing(&mut self, renew: Duration, ttl_ms: u64) {
        self.claim_renew = renew;
        self.claim_ttl_ms = ttl_ms;
    }

    /// Keep the approval ledger at `path` ([`approvals::default_path`] for
    /// the CLI and the host); `None` keeps none.
    pub fn set_approval_ledger(&mut self, path: Option<PathBuf>) {
        self.ledger_path = path;
    }

    /// The approval ledger this loop keeps, if any
    /// ([`Self::set_approval_ledger`], or a hosted loop's `opts.ledger`).
    #[must_use]
    pub fn approval_ledger(&self) -> Option<&Path> {
        self.ledger_path.as_deref()
    }

    /// Tests: the approval policy's process inputs.
    #[cfg(test)]
    pub(crate) fn set_approval_env(&mut self, env: ApprovalEnv) {
        self.approval_env = env;
    }

    /// The manager's session (`--inbox @sid`, else `@$ATERM_PARENT_SESSION_ID`):
    /// where `watch` posts its `kind=control` mail on a limit
    /// ([`Self::escalate`]). With none, the mail is skipped and the journal
    /// says so.
    pub fn set_manager(&mut self, manager: Option<String>) {
        self.manager = manager;
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
        self.clock = Some(now_ms.div_euclid(1000));
        self.clock_phase_ms = now_ms.rem_euclid(1000);
        self.local_offset = Some(local_offset_s);
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
    pub(crate) fn set_clock_jump(&self, jump: i64) {
        self.clock_jump
            .store(jump, std::sync::atomic::Ordering::SeqCst);
        self.clock_skew
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    /// Tests: the fixed clock RUNS from now, as the system's does. A loop
    /// that waits in real `Instant`s for a reset a few seconds off converts
    /// the reset from unix time at every reading
    /// ([`Self::wall_reset_at`]); a clock standing still would slide that
    /// target ahead of the loop for ever.
    #[cfg(test)]
    pub(crate) fn set_clock_running(&mut self) {
        self.clock_runs_from = Some(Instant::now());
    }

    /// Tests: a hold for a person ([`Self::held_for_person`]) is looked at
    /// again after `wait`, not after the rest of their grace. The hold is
    /// still decided on the whole grace, so a test stages a person well
    /// inside it — only a hang, never a loaded machine, ages them out of it
    /// before the decision — and the look that ends the hold reads their age
    /// from the next `status`, as the look at the grace's end does, with no
    /// grace waited out in real time.
    #[cfg(test)]
    pub(crate) fn set_person_hold_wait(&mut self, wait: Duration) {
        self.person_hold_wait = Some(wait);
    }

    /// Tests: the wake's settle has passed — it, and the due the Mac's wake
    /// placed at it ([`Self::woke`]), fall now — with none of it waited out
    /// in real time.
    #[cfg(test)]
    pub(crate) fn pass_wake_settle(&mut self) {
        let now = Instant::now();
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

    /// Tests: a zone table in place of `date`.
    #[cfg(test)]
    pub(crate) fn set_zone_lookup(&mut self, lookup: fn(&str, i64) -> Option<i64>) {
        self.zone_offset = lookup;
    }

    /// The turn-end policy's timings (tests shorten them; the host keeps
    /// the defaults). Starts the policy's memory of the session afresh.
    pub fn set_turn_end_timing(&mut self, timing: TurnEndTiming) {
        self.turn_end = TurnEndState::new(timing);
    }

    /// How long an outage is ridden out before the loop ends
    /// (`--reconnect-s`; 180 s unless set; zero: not at all).
    pub fn set_reconnect(&mut self, window: Duration) {
        self.reconnect = window;
    }

    /// How long the mail lane parks one `await inbox` before it re-arms (the
    /// loop's 20 s step unless set; a test shrinks it). The loop's process
    /// ends within one such step after its last line where the lane's client
    /// has no [`Ctl::interrupter`]; with one, the parked wait is cut short.
    #[cfg(test)]
    pub(crate) fn set_mail_step(&mut self, step: Duration) {
        self.mail_step = step;
    }

    /// How long a held idle point (`--mail`) waits for the worker's report
    /// between looks at the screen (the loop's 20 s step unless set; a test
    /// shrinks it) — the safety net under the hold.
    #[cfg(test)]
    pub(crate) fn set_hold_step(&mut self, step: Duration) {
        self.hold_step = step;
    }

    /// How long presses that miss are tried before the badge, shortened for
    /// a test.
    #[cfg(test)]
    pub(crate) fn set_press_badge_after(&mut self, after: Duration) {
        self.press_badge_after = after;
    }

    /// The survey's retry pauses, shortened for a test.
    #[cfg(test)]
    pub(crate) fn set_survey_retry(&mut self, pauses: [Duration; SURVEY_TRIES]) {
        self.survey_retry = pauses;
    }

    /// How long a rating card must have shown before its `0` goes ([`SURVEY_SETTLE`]),
    /// shortened for a test (a scripted server has no digit guard).
    #[cfg(test)]
    pub(crate) fn set_survey_settle(&mut self, settle: Duration) {
        self.survey_settle = settle;
    }

    /// How long the harness's `0` is given to be taken before a lone `0` under the open
    /// card is called dropped ([`SURVEY_LAND`]), set for a test: a scripted server has no
    /// clock, so a test picks the world (zero: the vendor's debounce has run).
    #[cfg(test)]
    pub(crate) fn set_survey_land(&mut self, land: Duration) {
        self.survey_land = land;
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
        self.probe_waits = ProbeWaits {
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
        self.reset_waits = waits;
    }

    /// How long a box must have shown before it is pressed ([`BOX_SETTLE`]),
    /// shortened for a test.
    #[cfg(test)]
    pub(crate) fn set_box_settle(&mut self, settle: Duration) {
        self.box_settle = settle;
    }

    /// How long the agent's background work must have stood before its host
    /// is offered the break ([`background_settle`]), shortened for a test.
    #[cfg(test)]
    pub(crate) fn set_background_settle(&mut self, settle: Duration) {
        self.background_settle = settle;
    }

    /// How long a save-then-wait switch may stand still through one break
    /// under a Codex background terminal before a person is told, shortened
    /// for a test.
    #[cfg(test)]
    pub(crate) fn set_switch_break_note(&mut self, bound: Duration) {
        self.switch_break_note = bound;
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
        // Nor does a stopped loop begin a wait. Some steps wait twice with no
        // stop check between (a press's settle, a refused `await inbox`), and
        // every wait goes through here, so a stop ends the run within one wait
        // (`run_hosted`: "within one wait of it").
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
        if let Some(sid) = &self.sid {
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
        // A press the server WROTE behind input the worker has not read (a
        // slow read, warned rather than refused): the next look reads
        // `status` before it presses again, as after a refusal
        // (`Session::back_off`) — so a worker the watch then publishes is
        // held before a second key queues behind the first.
        if claim::is_input(args) && r.warned_input_unread() {
            self.stall_suspect = true;
        }
        // Built only where the outage's books read it: almost never.
        let kind = || args[..args.len().min(2)].join(" ");
        if self.unserved(&r) {
            if self.outage.is_none() {
                let kind = kind();
                self.outage = Some(Outage {
                    since: Instant::now(),
                    kind,
                    told: false,
                    back: false,
                    pause: self.pause,
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
    /// read every turn, so a Codex worker is read as Codex.
    pub fn phase_now(&mut self) -> Result<(Turn, &'static dyn ScreenReader), String> {
        let screen = self.read_screen()?;
        let reader = self.program_reader(&screen.rows)?;
        Ok((self.turn_of(screen), reader))
    }

    /// [`Self::await_turn`] as `aterm drive await-turn` prints it: the
    /// session's program named first ([`Self::program_reader`]), so the wait
    /// and the detail lines go by its reader. A `status` that does not answer
    /// leaves the reader to the screen ([`Self::reader`]), as before the
    /// program is named: the wait itself rides a lost connection out.
    pub fn await_phase(
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
    /// caller outside the loops judges one screen of the session by (`drive
    /// task`'s nudge), so a Codex worker is read as Codex.
    pub(crate) fn program_reader(
        &mut self,
        rows: &[String],
    ) -> Result<&'static dyn ScreenReader, String> {
        self.program = self.foreground_program().map_err(String::from)?;
        Ok(self.reader(rows))
    }

    /// [`Self::reader`] for `rows`, the session's program named by `status`
    /// first where no read has named it yet (`aterm drive report`); a loop
    /// that has named it asks nothing more.
    pub(super) fn named_reader(
        &mut self,
        rows: &[String],
    ) -> Result<&'static dyn ScreenReader, Fail> {
        if self.program.is_none() {
            self.program = self.foreground_program()?;
        }
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
            Program::Claude | Program::Codex | Program::Agy | Program::Muse => false,
        };
        if self.program.is_none()
            && no_grammar
            && aterm_phase::parse_prompt_v2(rows).is_some_and(|p| p.head_off_screen)
        {
            return &aterm_phase::ClaudeReader;
        }
        by_screen
    }

    /// A row of `rows` matches the program's busy guard
    /// ([`ScreenReader::busy_guard`]): the turn's busy row is on the screen.
    /// `rows` is the live zone ([`live_rows`]), never the whole screen: a busy
    /// row above it is another process's, and would hold a wait armed on it.
    /// The guard is compiled once while it stays the same
    /// ([`Self::busy_match`]); one that does not compile matches nothing.
    ///
    /// "Up" is "not shown gone": a row the guard's scan could not decide (its
    /// budget ran out) counts as up, the same reading `await gone` gives it, so
    /// the turn is never let go on a busy row nobody read (ruling 2026-10-10 on
    /// `RowMatch::clears`).
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

    /// `screen` as a turn that ended, its phase read by [`Self::reader`].
    pub(super) fn turn_of(&self, screen: Screen) -> Turn {
        let reader = self.reader(&screen.rows);
        let mut phase = reader.phase(&screen.rows);
        // A wall waited on, its rows gone from the screen.
        if phase == Phase::Busy
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

    /// What makes `turn` the point it is ([`point_key`]), read by
    /// [`Self::reader`] — the ONE grammar every read of the loop goes
    /// through, the one [`Self::turn_of`] read its phase by and [`box_anchor`]
    /// its box by — never the reader the screen alone names, which may differ
    /// from the session's program. Every comparison of two points is of these
    /// keys: the badge's close ([`Self::close_box`]), the point already handed
    /// over ([`Self::look`]), the escalation's once per point
    /// ([`Self::escalate_point`]), the box settle, and the hold's look at the
    /// point still showing. The wait's look under a box's first row compares
    /// LESS: what a file box asks, never how anyone is answering it
    /// ([`Self::file_request`]).
    pub(super) fn review_key(&self, turn: &Turn) -> String {
        point_key(self.reader(&turn.screen.rows), turn)
    }

    /// What the FILE box on `turn` asks ([`file_request_key`]), read by
    /// [`Self::reader`] as [`Self::review_key`] is; `None` for any other
    /// point.
    fn file_request(&self, turn: &Turn) -> Option<String> {
        file_request_key(self.reader(&turn.screen.rows), turn)
    }

    /// The busy row a turn's FIRST wait waits to leave, before anything is
    /// read ([`Self::await_turn_from`]): the reader of the last screen read,
    /// over that screen's live zone ([`live_rows`]); with no screen read
    /// yet, the program's as named — with none named, Claude Code's `esc to
    /// interrupt` (every measured agent's busy row carries it) — over every
    /// row, UNSCOPED: no live zone is known, and a dead process's footer
    /// above it would hold the wait, so its step is capped
    /// ([`BusyWait::step`]).
    fn first_busy_wait(&self) -> Option<BusyWait> {
        let Some(screen) = &self.last else {
            let guard = self.reader(&[]).busy_guard().or_else(|| {
                self.program
                    .is_none()
                    .then(|| aterm_phase::Program::Claude.reader().busy_guard())
                    .flatten()
            })?;
            return Some(BusyWait {
                guard,
                span: None,
                unscoped: true,
            });
        };
        let reader = self.reader(&screen.rows);
        let guard = reader.busy_guard()?;
        let (_, span) = live_rows(reader, screen);
        Some(BusyWait {
            guard,
            span,
            unscoped: false,
        })
    }

    /// One read of the worker's screen over the control socket. Not named
    /// `read`: the lock-order census identifies locks by method name
    /// (`read`/`write`/`lock`/`try_*`), and a `read` held across another
    /// `read` looks like a re-entrant lock.
    fn screen(&mut self) -> Result<Screen, Fail> {
        let (screen, read_by) = self.read_once()?;
        if self.caps.await_agent.is_none() {
            self.caps.await_agent = Some(screen.generation.is_some());
        }
        let reader = read_by.unwrap_or_else(|| self.reader(&screen.rows));
        let open = reader.survey(&screen.rows);
        self.survey_gone |= !open;
        if !open {
            self.survey_first_seen = None;
        }
        // A Codex footer is kept on every read: the rate-limit nudge covers
        // it, and is answered by the model and the goal it showed before.
        // While the harness's own `/model` restore is in flight, its picker
        // is followed: once it has left, or a person has typed since the
        // `/model`, a picker on the screen is theirs.
        if reads_codex_state(reader.program()) {
            self.note_codex_screen(&screen.rows);
            if self.turn_end.wind().is_some_and(|w| w.restore_at.is_some()) {
                let shown = reader
                    .prompt(&screen.rows)
                    .is_some_and(|p| p.kind == aterm_phase::prompt::PromptKind::ModelPick);
                let now = Instant::now();
                let person = self.person_ago(now);
                self.turn_end.picker_seen(shown, person, now);
            }
        }
        if let Some(mode) = footer_mode(&screen.rows) {
            self.footer = Some(mode);
        } else if self.footer.is_none() && !self.busy_read {
            self.busy_read = reader.phase(&screen.rows) == Phase::Busy;
        }
        self.note_usage(reader, &screen.rows);
        // Every read clocks the draft: one that changed is a person typing.
        let draft = reader
            .composer(&screen.rows)
            .filter(|c| typed_draft_in(Some(c), reader.placeholder(), &screen))
            .map(|(_, lines)| lines.join("\n"));
        self.draft_seen = match (draft, self.draft_seen.take()) {
            (Some(d), Some((seen, at))) if d == seen => Some((seen, at)),
            (Some(d), _) => Some((d, Instant::now())),
            (None, _) => None,
        };
        // The last read kept in the previous one's buffers.
        let mut rows = self.last.take().map(|l| l.rows).unwrap_or_default();
        rows.clone_from(&screen.rows);
        self.last = Some(Screen {
            rows,
            generation: screen.generation.clone(),
            ..screen
        });
        Ok(screen)
    }

    /// What `rows` say of the account's usage, told to the host
    /// ([`IdleHost::usage_seen`]) when it changed since the last telling, or
    /// when it says something and that telling is
    /// [`crate::harness::accounts::CREDITS_RETELL_S`] old. A loop with no host
    /// reads nothing for it.
    fn note_usage(&mut self, reader: &dyn ScreenReader, rows: &[String]) {
        let Some(host) = self.stall_host.clone() else {
            return;
        };
        let seen = UsageSeen {
            credits: reader.credits(rows),
            warning: reader.usage_warning(rows),
        };
        let now = Instant::now();
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

    /// One read, and the reader it was read by when a tail that stands chose
    /// one ([`Self::reader`] of its rows; `None` for a whole read).
    fn read_once(&mut self) -> Result<(Screen, Option<&'static dyn ScreenReader>), Fail> {
        let mut zone = false;
        if self.caps.tail != Some(false) {
            let tail = format!("tail={TAIL_ROWS}");
            let r = self.call(&["text", "--json", &tail])?;
            if r.usage_error() {
                self.caps.tail = Some(false);
            } else if r.ok() {
                self.caps.tail = Some(true);
                let screen = parse_text_json(&r.stdout).map_err(Fail::Hard)?;
                let reader = self.reader(&screen.rows);
                let gap = gap_above_tail(&screen);
                if !tail_misses_the_live_rows(reader, &screen)
                    && !drawn_above_tail(reader, &screen)
                    && !gap
                {
                    return Ok((screen, Some(reader)));
                }
                // The tail cut a box's head off, holds none of the live
                // rows, ends on a blank row under drawn rows and decides
                // nothing or an idle composer, or holds a gap the live zone
                // reaches over: the full read below — cut to the live zone,
                // the rows the server's verdict reads, where the tail ended
                // on a blank row or held a gap, unless that zone misses the
                // live rows too.
                zone = gap || screen.rows.last().is_some_and(|r| r.trim().is_empty());
            } else {
                return Err(self.fault(
                    &r,
                    format!("text --json {tail} failed: {}", r.stderr.trim()),
                ));
            }
        }
        let r = self.call(&["text", "--json"])?;
        if !r.ok() {
            return Err(self.fault(&r, format!("text --json failed: {}", r.stderr.trim())));
        }
        let whole = parse_text_json(&r.stdout).map_err(Fail::Hard)?;
        Ok((if zone { self.live_zone(whole) } else { whole }, None))
    }

    /// A whole read cut to the screen's live zone — its last [`TAIL_ROWS`]
    /// drawn rows, a blank gap counting as one, and the blank rows under them
    /// ([`aterm_phase::live_zone_start`]), the rows the server's agent verdict
    /// reads — or left whole where that cut would miss the live rows as a
    /// tail can ([`tail_misses_the_live_rows`]): it takes a box's head off
    /// (the server reads that box whole too), or the cursor's row is above it.
    fn live_zone(&self, mut whole: Screen) -> Screen {
        let first = aterm_phase::live_zone_start(&whole.rows, TAIL_ROWS);
        // Cut in place; a cut that misses puts the rows above back.
        let mut above: Vec<String> = whole.rows.drain(..first).collect();
        let whole_first = std::mem::replace(&mut whole.first, first);
        if tail_misses_the_live_rows(self.reader(&whole.rows), &whole) {
            above.append(&mut whole.rows);
            whole.rows = above;
            whole.first = whole_first;
        }
        whole
    }

    /// ONE WHOLE READ OF THE GRID (`text --json`, no `tail=`) WITH NONE OF
    /// [`Self::screen`]'s BOOKKEEPING: nothing is kept of it — not the last
    /// screen, the footer's mode, the draft's clock, the survey's leaving or
    /// a Codex footer. The accounts probe reads the Settings panel so
    /// ([`Self::usage_probe`], [`Self::usage_panel_look`]): the panel stands
    /// where the composer was, is the harness's own doing and none of the
    /// worker's, and its rule and header are on rows 2 and 3 of the grid —
    /// above any tail. A wall's decision reads the whole screen so for the
    /// call a tail can cut off above the wall
    /// ([`Self::under_call_on_screen`]). The outage's books are
    /// [`Self::call`]'s, as for every request.
    pub(super) fn grid(&mut self) -> Result<Screen, Fail> {
        let r = self.call(&["text", "--json"])?;
        if !r.ok() {
            return Err(self.fault(&r, format!("text --json failed: {}", r.stderr.trim())));
        }
        parse_text_json(&r.stdout).map_err(Fail::Hard)
    }

    /// One `await <cond> timeout <step>`. A request not served is told apart
    /// before the exit code is read: the client's own deadline also exits 124,
    /// and only the server's `OK timeout` is a step that ran out.
    fn wait(&mut self, cond: &[&str], step: Duration) -> Result<Wait, Fail> {
        // Rounded up to whole milliseconds, so a wait is never shorter than
        // asked (a truncated box settle woke early and read the box as young).
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
    fn wait_gone(&mut self, busy: &BusyWait, step: Duration) -> Result<Wait, Fail> {
        // The wait's arguments after `await`: `gone <guard> [rows <a> <b>]`.
        let span = busy.span.map(|(a, b)| (a.to_string(), b.to_string()));
        let mut cond = vec!["gone", busy.guard.as_str()];
        if let Some((a, b)) = &span {
            cond.extend(["rows", a.as_str(), b.as_str()]);
        }
        match self.wait(&cond, step) {
            Err(Fail::Hard(e)) if busy.span.is_some() && e.contains("ERR bad rows") => {
                Ok(Wait::Latched)
            }
            other => other,
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

    /// Block until the worker's phase is no longer busy, or `timeout` passes.
    ///
    /// The loop: wait; read; done unless busy; else wait again. Where the
    /// server knows `await gone`, the wait after a busy read showing the busy
    /// footer is the footer leaving (`await gone esc.to.interrupt`): the turn
    /// ending and a box drawn where the footer was both latch it at once,
    /// however the screen above animates. That wait watches the live zone of
    /// the read that armed it (`rows <a> <b>`, [`live_rows`]), never a footer
    /// above it (a dead process's footer never leaves). A busy read without
    /// the footer in its live zone waits for the next content change and a
    /// short settle ([`SETTLE_MS`], at most [`SETTLE_CAP`]); a turn's first
    /// wait is the footer leaving (armed before any read, over every row,
    /// capped by [`UNSCOPED_CAP`]) or that settle right after a press or a
    /// review point. A build without `await gone` settles with `await idle
    /// 2000`, capped at 20 s a step. A screen without Claude Code's composer
    /// frame that never held still for 2 s and shows no box is output still
    /// arriving and counts as busy. An outage is ridden out ([`Self::ride_out`],
    /// lines on stderr) and the wait restarts from a fresh read; a timeout spent
    /// in one is the timed-out turn on the last screen read (`spent_turn`).
    pub fn await_turn(&mut self, timeout: Duration) -> Result<Turn, String> {
        self.await_turn_to(timeout, &mut std::io::stderr())
    }

    /// [`Self::await_turn`], its `RECONNECT …` lines said to `log`.
    fn await_turn_to(&mut self, timeout: Duration, log: &mut dyn Write) -> Result<Turn, String> {
        let deadline = Instant::now() + timeout;
        let mut gone_first = true;
        let mut moved = Saw::default();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            // `--context-warn` is the loops' (`drive` arms it): `await-turn`
            // has no context line to say.
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

    /// [`Self::await_turn`], with the `await gone` first wait optional: right
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
    /// and step nothing: no Escape into a running Codex turn
    /// ([`Self::stop_codex_turn`]), none of the save-then-wait switch's steps
    /// at a background break ([`Self::switch_at_background`]), no host step
    /// there ([`Self::host_steps_in_background`]), and no rating card
    /// dismissed ([`SurveyDuty`], which the look gives no Off loop). The turn
    /// is waited out as any is, and the look parks at its point
    /// ([`Self::park_while_off`]); what the switch had in hand stands until
    /// the word turns (the design's §7).
    fn await_turn_from(
        &mut self,
        timeout: Duration,
        gone_first: bool,
        saw: &mut Saw,
        mut duty: Option<&mut SurveyDuty<'_>>,
        opts: Option<&SuperviseOpts>,
        review: &mut dyn Review,
    ) -> Result<Turn, Fail> {
        let deadline = Instant::now() + timeout;
        let mut next = match gone_first.then(|| self.first_busy_wait()).flatten() {
            Some(busy) => Next::Gone(busy),
            None => Next::Settle,
        };
        // Since when every busy read showed the agent's own background work
        // and nothing else ([`Self::host_steps_in_background`]).
        let mut background_since: Option<Instant> = None;
        // The `Gone` in hand was armed on a busy row a read of this wait SAW
        // (not the first wait's guess from before any read).
        let mut footer_seen = false;
        loop {
            // The hosted loop's stop ends a turn that may run for hours:
            // the look it returns to says so ([`Self::look`]). Checked here
            // and before each of the turn's other waits (after a refused
            // `await gone`, before the wait for the next change), so a stop
            // in any wait of the turn ends the turn, spent, rather than as
            // the refusal of its next wait ([`Self::call`]).
            if self.stopped() {
                return Ok(self.spent_turn());
            }
            let step = deadline
                .saturating_duration_since(Instant::now())
                .min(next_wait_step());
            // A wait that said nothing about the screen holding still: the
            // footer leaving, or the short settle.
            let mut unsettled = false;
            // The screen held still for IDLE_MS right before the read.
            let mut settled = false;
            let modern = self.caps.gone != Some(false);
            match &next {
                Next::Gone(busy) if modern => match self.wait_gone(busy, busy.step(step))? {
                    Wait::Unsupported => {
                        self.caps.gone = Some(false);
                        if self.stopped() {
                            return Ok(self.spent_turn());
                        }
                        settled = matches!(self.wait(&["idle", IDLE_MS], step)?, Wait::Latched);
                    }
                    _ => {
                        self.caps.gone = Some(true);
                        unsettled = true;
                    }
                },
                Next::Settle if modern => {
                    self.wait(&["idle", SETTLE_MS], step.min(SETTLE_CAP))?;
                    unsettled = true;
                }
                // A screen without the composer frame is settled by the full
                // idle window, re-read every few seconds: output still
                // arriving never holds still, and the program writing it may
                // be the agent drawing its first frame, whose footer then
                // takes over the wait.
                Next::Idle if modern => {
                    settled = matches!(
                        self.wait(&["idle", IDLE_MS], step.min(WRITING_CAP))?,
                        Wait::Latched
                    );
                }
                Next::Gone(_) | Next::Settle | Next::Idle => {
                    settled = matches!(self.wait(&["idle", IDLE_MS], step)?, Wait::Latched);
                }
            }
            let mut screen = self.screen()?;
            let reader = self.reader(&screen.rows);
            if Instant::now() < deadline {
                self.watch_context(&screen, reader, &mut |line| review.say(line))?;
            }
            let mut phase = reader.phase(&screen.rows);
            // No box on the screen: the box before it has left, and the one
            // after it mounts afresh.
            if phase != Phase::Prompt {
                self.box_first_seen = None;
            }
            // No composer and the screen never held still: a build's or a
            // REPL's output still arriving, not the end of anything. NOT
            // while a `/usage` panel of the harness's is owed its close
            // ([`UsageProbe`]): the panel stands where the composer was and
            // repaints as it loads, and read as output being written it
            // would be the worker working — a turn that ran, work for the
            // policy's back-off — with the look that closes it never
            // reached while it moves. Such a read is handed to the look as
            // it is ([`Self::usage_panel_look`], which makes no decision on
            // it). Nor while a limit reset's step is owed ([`ResetStep`]):
            // its pending lines stand where the composer was, under a
            // dialog's title, and are the look's to read
            // ([`Self::reset_look`]).
            let writing = !settled
                && !matches!(phase, Phase::Busy | Phase::Prompt)
                && reader.composer(&screen.rows).is_none()
                && self.probe == UsageProbe::Idle
                && self.reset == ResetStep::Idle;
            // A dialog no reader names whose screen never holds still (a tool
            // row blinking over an unrecognised box) is `writing` for ever and
            // never reaches the wait that would see it.
            if writing {
                self.dialog_note(&screen, review);
            }
            if phase != Phase::Busy && !writing {
                self.switch_break = None;
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
            // frames: a switch, a host's background point or a rating card.
            // A refused mode read keeps the books but admits no action.
            let may_act = !writing
                && (self.turn_end.switch_open()
                    || (self.stall_host.is_some()
                        && reader.background_wait(&screen.rows).is_some())
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
            // Codex's save-then-wait switch: a turn's end under a background
            // terminal is a point for the switch's own steps, and nothing
            // else ([`Self::switch_at_background`]) — no host step, no work.
            // None under Off: the switch stands as it is.
            let switch_break = match opts {
                Some(opts) if !writing && !off => {
                    self.switch_at_background(&screen, opts, review)?
                }
                _ => {
                    self.switch_break = None;
                    false
                }
            };
            // A wall the loop waited on whose rows have left the screen (a
            // scroll back through the transcript, a local command's output
            // under it) while only the agent's own background work holds it
            // busy: the point is the wall still, and the standing wait goes
            // on. A live turn ends the memory.
            if phase == Phase::Busy && !writing {
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
                self.end_memory_on_live_turn(reader, &screen.rows, screen.seq);
            }
            // THE AGENT'S LIVE TURN — its spinner or busy footer, never a
            // break of its own background work: the loop's act awaited, if
            // any, was taken ([`TurnEndState::act_untaken`]), so a break it
            // leads to is the agent's own and may be offered to the host. (A
            // turn's end the save-then-wait switch took as its point is no
            // live turn.)
            if !writing && !switch_break && reader.background_wait(&screen.rows).is_none() {
                self.turn_end.took();
                // A refused wall's episode is over: met again at a later
                // point, it is said again ([`Self::say_wall_refused`]).
                self.wall_refusal_over();
            }
            // A break of the agent's own background work is offered to the
            // host; whatever it types there, the wait below is for the screen
            // to move on from this read.
            if !writing && !switch_break {
                if !off {
                    self.host_steps_in_background(&screen, &mut background_since, review);
                }
                self.stuck_busy_note(&screen, background_since, review);
            }
            if !(writing && unsettled) && !switch_break {
                saw.busy = true;
                saw.live |= !writing;
                // The worker works: the wait the host was told ended (the
                // try was typed) — told here, not at the next point, which
                // the busy turn keeps this loop from reaching.
                if let Some(opts) = opts {
                    self.end_told_wait(opts);
                }
                // The turn's work, as the turn-end policy measures it.
                self.running.busy(Instant::now());
                self.last_busy_at = Some(Instant::now());
                // `--mail`: the turn has begun by now (`MailIn::busy_at`).
                if let Some(m) = &mut self.mail
                    && m.busy_at.is_none()
                {
                    m.busy_at = Some(Instant::now());
                }
                // Unattended: the worker works, so a self-resuming limit notice
                // is over, and so is the point this loop escalated (a person
                // answered and the worker took it). Its badge goes now, at the
                // first busy read, since the turn it started may run for hours.
                // A screen only `writing` is no agent's read: it waits for one.
                if review.unattended() {
                    self.limit_on_busy(screen.seq, review)?;
                    // A person's point the worker has gone back to work on is
                    // over — unless the loop is still waiting on a wall whose
                    // rows are out of view, which is no answer to it, or the
                    // spinner is its own `/compact`'s run: the point a
                    // compaction that fails leaves is the same one.
                    if self.wall_memory.is_none() && !self.turn_end.compacting() {
                        self.exception_ended(screen.seq, Some("the agent working"), review);
                    }
                    if self.box_ask.is_some() && !writing {
                        self.close_box(screen.seq, review)?;
                    }
                    // Likewise the told choice: a dialog the loop answered is
                    // done and is told now (`CHOSE`, its chime and rim pulse),
                    // naming this read, since the turn may run for hours and a
                    // loop stopped in it would never tell it. The look's own
                    // edge is the fallback for a turn read busy only as
                    // `writing`.
                    if !writing && !self.question.answered.is_empty() {
                        self.question_done(
                            screen.seq,
                            opts.and_then(|o| o.notes.as_deref()),
                            review,
                        )?;
                    }
                    // Codex's save-then-wait switch: a turn running while it
                    // is open is stopped (no work goes on on the cheaper model)
                    // — never under Off, where the turn is the person's or
                    // the agent's own and the switch stands until the word
                    // turns (the design's §7).
                    if !writing && !off {
                        self.stop_codex_turn(&screen, review)?;
                    }
                    // A limit reset's step still owed: the worker works, so
                    // its dialog is gone, and the host is told now — the turn
                    // may run for hours, and the account's other sessions
                    // wait on that word ([`Self::reset_at_work`]).
                    if !writing && let Some(opts) = opts {
                        let live = !switch_break && reader.background_wait(&screen.rows).is_none();
                        self.reset_at_work(&screen, live, opts, review)?;
                    }
                }
                // The agent's memory past its ceiling while it works: a
                // warning, said once an episode (and again when its verdict
                // changes); any restart waits for the turn's idle point, and
                // only the Mac's trouble makes one.
                // A restart refused at an earlier point still names the
                // person's ([`Self::memory_refused`]): the next idle point
                // asks again, and is mostly refused the same (a launch-bound
                // refusal stands for the agent's life; some do not).
                if !writing
                    && let Some(opts) = opts
                    && let Some(over) = self.memory_measure(opts)
                {
                    self.memory_warn(&screen, over, opts, review);
                }
            }
            if Instant::now() >= deadline {
                return Ok(Turn {
                    phase,
                    screen,
                    timed_out: true,
                });
            }
            // A rating card parked over a worker that reads BUSY (a spinner, a
            // workflow it waits on for an hour) is dismissed here, on the read that
            // shows it, by the same step the idle look takes ([`Self::survey`]) —
            // never a box (that is `Prompt`, not busy), never a draft or a person's
            // grace. A press asks to read again before anything else: the wait
            // settles, and the next read judges it.
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
                // Settle first. The read right after a footer this wait
                // saw has left is often the next frame mid-draw (a question
                // dialog's first rows, no composer yet), whole within the
                // short settle: it gets that first. Any other, and a screen
                // still writing after the settle, waits out the full idle
                // window.
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
            if let Some(guard) = reader.busy_guard().filter(|busy| {
                !writing
                    && self.caps.gone != Some(false)
                    && self.busy_up(busy, &screen.rows[live..])
            }) {
                // Level-triggered: it latches the moment the busy row leaves.
                next = Next::Gone(BusyWait {
                    guard,
                    span,
                    unscoped: false,
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

    /// `--context-warn` at one read: the context indicator
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
    /// A CODEX whose screen draws no figure — 0.163.0's default footer draws
    /// none — is read by its rollout's ([`super::codex_usage::CodexSeen::
    /// context_left`]: the last response's tokens against the model's
    /// window, as Codex draws its own), on the records look's own clock
    /// ([`Self::codex_records`]), while no box is up. Its compaction shows as
    /// the figure's rise (a compacted thread reads near 100% left); a figure
    /// that cannot be read is no sign it has gone, and neither is a Codex
    /// screen without one, since Codex's own item stays drawn through a
    /// compaction.
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
        // Only Codex's records carry a context figure aterm reads.
        let codex = match reader.program() {
            Program::Codex => true,
            Program::Claude | Program::Agy | Program::Muse | Program::Generic => false,
        };
        let left = match reader.context(rows) {
            Some(left) => Some(left),
            // A box up covers where a Codex that draws its figure (an empty
            // `tui.status_line`) draws it: the records' figure, up to a look's
            // clock old, is then never weighed against one the screen drew.
            None if codex && reader.prompt(rows).is_none() => self.codex_records().context_left,
            None => None,
        };
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
                    None => {
                        !codex && reader.composer(rows).is_some() && reader.prompt(rows).is_none()
                    }
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

    /// The loop. Returns the text to print and the exit code: `0` with the
    /// compact result when the worker needs the manager (a non-read prompt, a
    /// question, a limit notice, an idle composer); [`EXIT_TIMEOUT`] with
    /// `TIMEOUT` and the last read's compact result when the budget is spent —
    /// a turn read at or after the deadline is the TIMEOUT, never pressed, and
    /// so is a budget spent while an outage is ridden out. An outage is ridden
    /// out ([`Self::ride_out`]), its lines on stderr, never in the result; so
    /// are the survey's and the context indicator's `EVENT` lines.
    pub fn supervise(&mut self, opts: &SuperviseOpts) -> Result<(String, u8), String> {
        self.supervise_to(opts, &mut std::io::stderr())
    }

    /// [`Self::supervise`] with `--mail`'s lane (`lane`: the second client
    /// the lane parks on the manager's inbox; `None` runs without one, as
    /// [`Self::supervise`] does). Every `MAIL` line goes to stderr with the
    /// `RECONNECT` lines; an idle review point is held for the worker's
    /// report (`Session::hold_for_report`) and the result's phase lines end
    /// with `report <id> rows=<n>`, or `report -` when none came in the
    /// grace ([`render_result_mail`]); the journal gets the `EVENT turn` or
    /// `EVENT idle-no-report` line `watch --mail` would print.
    pub fn supervise_mail<L: Ctl + Send>(
        &mut self,
        opts: &SuperviseOpts,
        lane: Option<&mut L>,
    ) -> Result<(String, u8), String> {
        self.supervise_with(opts, lane, &mut std::io::stderr())
    }

    /// [`Self::supervise`], its `RECONNECT …` lines said to `log`. With
    /// `--journal`, the file gets every line said on `log`, an `APPROVED`
    /// line per approval and the `EVENT` line `watch` would print for the
    /// review point, then `TIMEOUT`, or `EXIT <reason>` for the error the
    /// loop ends on; a journal that fails is said once on `log`.
    fn supervise_to(
        &mut self,
        opts: &SuperviseOpts,
        log: &mut (dyn Write + Send),
    ) -> Result<(String, u8), String> {
        self.supervise_with(opts, None::<&mut NoLane>, log)
    }

    /// [`Self::supervise_to`] with the mail lane.
    fn supervise_with<L: Ctl + Send>(
        &mut self,
        opts: &SuperviseOpts,
        lane: Option<&mut L>,
        log: &mut (dyn Write + Send),
    ) -> Result<(String, u8), String> {
        let allow = opts.allow();
        // `report` counts into `watch`'s one line; `supervise` prints the rows
        // themselves, so it reads none.
        let opts = &SuperviseOpts {
            report: false,
            ..opts.clone()
        };
        let journal = Journal::open(opts.journal.as_deref(), self.sid.as_deref(), log);
        let sink = Mutex::new(Sink {
            out: log,
            journal,
            warn: None,
            // `supervise` returns to a manager who decides; only `watch`, the
            // loop that decides by itself, tells the window.
            teller: None,
        });
        let end = self.run_loop(
            opts,
            &sink,
            lane,
            &mut StopAtReview {
                sink: &sink,
                allow: &allow,
            },
        );
        match end {
            Ok(End::Stopped(turn, fold)) => {
                let reader = self.reader(&turn.screen.rows);
                Ok((render_result_mail(&turn, reader, &allow, fold), 0))
            }
            Ok(End::Timeout(turn)) => {
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .record("TIMEOUT", None);
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
                    .record(&format!("EXIT {}", exit_reason(&e)), None);
                Err(e)
            }
        }
    }

    /// [`Self::drive`], with the mail lane running beside it when `opts.mail`
    /// asks for one and `lane` is a client for it: the lane's thread parks
    /// `await inbox` on the manager's session ([`mail::Lane::run`]), prints
    /// through `sink`, and hands the watched worker's reports to
    /// `Session::hold_for_report`. The lane is told to stop when the loop
    /// ends, its parked wait cut short where its client can be interrupted
    /// ([`Ctl::interrupter`]: `aterm-ctl` is signalled), and joined — at
    /// once, or within one parked wait (`mail_step`) where it cannot.
    fn run_loop<L: Ctl + Send>(
        &mut self,
        opts: &SuperviseOpts,
        sink: &Mutex<Sink<'_>>,
        lane: Option<&mut L>,
        review: &mut dyn Review,
    ) -> Result<End, String> {
        let Some((mopts, lane)) = opts.mail.as_ref().zip(lane) else {
            self.mail = None;
            return self.drive(opts, review);
        };
        let (tx, rx) = mpsc::channel();
        self.mail = Some(MailIn {
            rx,
            pending: Vec::new(),
            window: mopts.report_window,
            grace: mopts.idle_grace,
            gone: false,
            floor: Instant::now(),
            busy_at: None,
            handed: false,
        });
        let worker = self.sid.clone();
        // Taken before the lane's thread owns the client: what cuts its
        // parked wait short once the loop has ended.
        let cut = lane.interrupter();
        let lane = mail::Lane {
            ctl: lane,
            inbox: mopts
                .inbox
                .clone()
                .unwrap_or_else(|| mail::SELF.to_string()),
            step: self.mail_step,
            reconnect: self.reconnect,
            pause: self.pause,
            pause_max: self.pause_max,
        };
        let stop = AtomicBool::new(false);
        let say = |line: &str| {
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .line(line, None)
        };
        let end = std::thread::scope(|s| {
            s.spawn(|| lane.run(worker.as_deref(), &say, tx, &stop));
            let end = self.drive(opts, review);
            stop.store(true, Ordering::Relaxed);
            if let Some(cut) = &cut {
                cut();
            }
            end
        });
        self.mail = None;
        end
    }

    /// `supervise`'s loop for a harness that wakes its agent once per stdout
    /// line (a background monitor, a supervisor process). A review point prints
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
    /// lapsed: <the last failure>`) or the loop fails (a request, the notes
    /// file). Every line is flushed as it is written. With
    /// [`SuperviseOpts::report`], an idle, question or limited point's line
    /// carries `complete=<0|1> rows=<n>` of the report read at that point
    /// ([`reported_event_line`]); without it the line is [`event_line`]'s. With
    /// [`SuperviseOpts::context_warn`] above 0, Claude Code's context indicator
    /// is watched on every read of a turn, a busy one included: `EVENT context
    /// seq=<n> <v>% until auto-compact` when it first reads at or below the
    /// threshold, once a descent, then `EVENT compacted seq=<n>` once it has
    /// gone (or jumped back up) — the worker compacted, and the watch is armed
    /// again.
    #[cfg(test)]
    pub(crate) fn watch(&mut self, opts: &SuperviseOpts, out: &mut (dyn Write + Send)) -> u8 {
        self.watch_to(opts, out, &mut std::io::stderr())
    }

    /// `Self::watch` with `--mail`'s lane (`lane`: the second client parked on
    /// the manager's inbox; `None` runs without one). The lane prints `MAIL
    /// id=<n> off=<o> from=<sid> kind=<k> len=<n> [re=<o>]` per delivery on
    /// stdout; an idle point is held for the worker's report
    /// (`Session::hold_for_report`) and printed as `EVENT turn seq=<n>
    /// report=<id> rows=<n> <summary>`, or `EVENT idle-no-report seq=<n>
    /// <summary>` once the grace is spent. When mail cannot be watched it says
    /// `MAIL off: <why> (the loop goes on without mail)` once.
    #[cfg(test)]
    pub(crate) fn watch_mail<L: Ctl + Send>(
        &mut self,
        opts: &SuperviseOpts,
        lane: Option<&mut L>,
        out: &mut (dyn Write + Send),
    ) -> u8 {
        self.watch_with(opts, lane, None::<&mut NoLane>, out, &mut std::io::stderr())
    }

    /// `Self::watch_mail` with the STORY lane (`teller`: a second client
    /// the loop tells the worker's window its decisions through — `aterm ctl
    /// @<sid> story <verb> [<text>]` for every journaled line that carries
    /// one, in the journal's order, never the command; see [`story_of`]).
    /// The printed lines are byte-identical with or without it: a story is a
    /// side channel, and a host that lacks the verb changes nothing here.
    pub fn watch_telling<L: Ctl + Send, S: Ctl + Send>(
        &mut self,
        opts: &SuperviseOpts,
        lane: Option<&mut L>,
        teller: Option<&mut S>,
        out: &mut (dyn Write + Send),
    ) -> u8 {
        self.watch_with(opts, lane, teller, out, &mut std::io::stderr())
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
        self.watch_with(opts, None::<&mut NoLane>, None::<&mut NoLane>, out, warn)
    }

    /// [`Self::watch_to`] with the mail lane and the story lane.
    fn watch_with<L: Ctl + Send, S: Ctl + Send>(
        &mut self,
        opts: &SuperviseOpts,
        lane: Option<&mut L>,
        teller: Option<&mut S>,
        out: &mut (dyn Write + Send),
        warn: &mut (dyn Write + Send),
    ) -> u8 {
        let allow = opts.allow();
        let journal = Journal::open(opts.journal.as_deref(), self.sid.as_deref(), warn);
        let sid = self.sid.clone();
        let sink = Mutex::new(Sink {
            out,
            journal,
            warn: Some(warn),
            teller: teller.map(|ctl| Teller {
                ctl: ctl as &mut (dyn Ctl + Send),
                sid,
            }),
        });
        let mut review = Lines {
            sink: &sink,
            allow: &allow,
        };
        let end = self.run_loop(opts, &sink, lane, &mut review);
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
            .line(&line, None);
        code
    }

    /// The loop `supervise` and `watch` share: await the turn; under the
    /// approval policy, answer a box ([`Self::auto_read`]); anything else is a
    /// review point for `review`, which stops the loop or reports it and keeps
    /// watching, after which the loop waits for the screen to move past it. An
    /// unserved request is ridden out ([`Self::ride_out`]) and the loop looks
    /// again from a fresh read; budget spent there is the loop's TIMEOUT.
    fn drive(&mut self, opts: &SuperviseOpts, review: &mut dyn Review) -> Result<End, String> {
        let end = self.drive_loop(opts, review);
        // The loop's end ends what it told of a person's point: a host that
        // filed it retires it, and the journal says how long it stood.
        self.stuck_over(0, "the loop ended", review);
        self.exception_ended(0, Some("the loop ended"), review);
        end
    }

    /// [`Self::drive`]'s body.
    fn drive_loop(&mut self, opts: &SuperviseOpts, review: &mut dyn Review) -> Result<End, String> {
        // Mutable: a limit notice naming a reset past it stretches it
        // (`extend`), once a reset.
        let mut deadline = deadline_after(opts.max);
        let allow = opts.allow();
        self.stall_host.clone_from(&opts.idle_host);
        self.human_grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
        self.stall_grace_s = opts.policy.human_grace_s;
        self.stall_term_after_s = opts.policy.stall_term_after_s;
        self.stall_term = opts.policy.stall_term;
        // Every read of this loop's turns is shown to the context watch
        // (`await_turn_from`), armed from the start.
        self.context_warn = opts.context_warn;
        self.context = Context::Armed;
        if !self.turn_end_seeded
            && let Some(path) = self.ledger_path.as_deref()
        {
            approvals::bound(path);
        }
        self.ledger = Ledger::open(
            self.ledger_path.as_deref(),
            self.sid.as_deref(),
            &mut std::io::stderr(),
        );
        let mut open_wall = None;
        // The typing budget is the session's, not this loop's: the acts a
        // previous loop on it typed within the window count, so a loop
        // re-spawned by its host does not start with a fresh budget.
        if !std::mem::replace(&mut self.turn_end_seeded, true)
            && let Some(path) = self.ledger_path.as_deref()
        {
            let now_ms = super::journal::unix_ms();
            // (A made account move's row is `typed` and no typed act:
            // [`approvals::typed_at`] leaves it out.)
            let ages: Vec<Duration> = approvals::typed_at(path, self.sid.as_deref())
                .into_iter()
                .filter_map(|ts| u64::try_from(now_ms - ts).ok())
                .map(Duration::from_millis)
                .collect();
            self.turn_end.seed_acts(&ages, Instant::now());
            // A loop the host started as the session's Off lifted: the person
            // drove the session since those acts, so they carry no task —
            // but for a turn already running as the loop attaches
            // ([`TurnEndState::start_lifted_off`]).
            if opts.idle_host.as_ref().is_some_and(|h| h.lifted_off()) {
                self.turn_end.start_lifted_off();
            }
            // The usage act a previous loop made at the wall this one
            // attaches to (a new window, a seamless update, a fault) is this
            // loop's own last try — its next is the back-off from THAT act,
            // not a second `keep going` the moment it attaches. Not one typed
            // into an account the session has since LEFT
            // ([`approvals::usage_attempt_at`]).
            let resumed = approvals::usage_attempt_at(path, self.sid.as_deref());
            if let Some(ago) = resumed
                .and_then(|ts| u64::try_from(now_ms - ts).ok())
                .map(Duration::from_millis)
            {
                self.turn_end.seed_usage_attempt(ago, Instant::now());
            }
            // …and it answered every clearing of the owner's made before it
            // (`accounts use` on the account the session stood at the limit
            // of): that continuation is typed once a clearing, by whichever
            // loop.
            if let Some(ts) = resumed {
                self.turn_end.seed_account_resumed(ts.div_euclid(1000));
            }
            // The wall an earlier loop waited on, when the screen shows none
            // of its rows (the scroll that hid it, then the update that
            // applied): waited on still (seeded below).
            open_wall = approvals::open_wall(path, self.sid.as_deref());
            // A model switch an earlier loop made is switched back at its
            // reset by this one.
            if let Some(open) = approvals::open_model_switch(path, self.sid.as_deref()) {
                let switch = self.carried_model_switch(open);
                self.turn_end.seed_model(switch);
            }
            // The account moves an earlier loop made count toward this
            // one's bound (240 relaunches in five days is what a bound that
            // starts over with each loop allows); a move it could not make
            // is not asked again at the wall it left standing; and one the
            // ledger does not show SERVED is this loop's to answer — with a
            // turn already asked under it, or a bad answer already told,
            // where the ledger says so; made at a wall with neither, it
            // still owes that wall's resume at the notice it left
            // ([`TurnEndState::seed_account`]).
            let moves = approvals::account_moves(path, self.sid.as_deref());
            let ago = |ts: i64| u64::try_from(now_ms - ts).ok().map(Duration::from_millis);
            let made: Vec<Duration> = moves.made_ms.iter().filter_map(|&ts| ago(ts)).collect();
            if !made.is_empty() {
                self.turn_end.seed_account_moves(&made, Instant::now());
            }
            if moves.unmade {
                self.turn_end.seed_account_unmade();
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
                    Instant::now(),
                );
            }
            // A Codex save-then-wait switch an earlier loop opened is carried
            // on from where its last row left it: its wind-down, its restore,
            // its hold until the reset.
            if let Some(open) = approvals::open_wind_down(path, self.sid.as_deref()) {
                let now = Instant::now();
                let wind = self.carried_wind(open, now);
                self.turn_end.seed_wind(wind, now);
            }
            // A `/usage` panel an earlier loop wrote for and no loop has
            // seen gone ([`approvals::probe_owed`]: the loop died, or was
            // stopped, between its write and the panel's close) is this
            // loop's to close — after one person's grace in its own sight,
            // and with nothing sampled from it. With no such row — every
            // ledger where no probe was ever typed — nothing is owed, and
            // this loop closes no panel it finds.
            // One read of the ledger answers this and the reset's question
            // below.
            let body = std::fs::read_to_string(path).ok();
            if body
                .as_deref()
                .is_some_and(|b| approvals::probe_owed(b, self.sid.as_deref()))
            {
                self.probe = UsageProbe::Owed {
                    own: false,
                    since: Instant::now(),
                };
            }
            // A limit reset's row an earlier loop took and no loop has told
            // the end of ([`approvals::reset_owed`]: the loop died, or was
            // stopped, inside its step) is this loop's to end — and NEVER to
            // enter: a confirmation still up is Escaped after one person's
            // grace in this loop's own sight ([`Self::reset_look`]). With no
            // such row — every ledger where no reset's row was ever taken —
            // nothing is owed, and a confirmation this loop finds is a
            // person's. Whatever `[harness] limit_reset` says: the key
            // decides whether a row is TAKEN, never whether an open one is
            // ended. With it off, or for a row OLDER than a step can be
            // ([`reset_loop::RESET_STALE`]: no loop ran since), the step is
            // Escape-only as any successor's and ends untold
            // ([`Self::reset_end`]). Only by a loop that makes the step's
            // look: an attended one could not end it, and leaves the row.
            if review.unattended()
                && let Some(open) = body
                    .as_deref()
                    .and_then(|b| approvals::reset_owed(b, self.sid.as_deref()))
            {
                self.reset_take_up(&open, review);
            }
        }
        self.seed_wall_memory(open_wall);
        let mut state = Looking {
            approved: Vec::new(),
            gone_first: true,
            handed: None,
            moved: Saw::default(),
            survey: Survey::Closed,
            carried: None,
            read_back: None,
        };
        // An attention a previous watcher left is settled before the first
        // wait: cleared when the screen no longer shows its point, adopted
        // when it does. A request not served here is the first look's to
        // ride out.
        if review.unattended() {
            self.take_claim(review);
            if let Some(held) = self.yield_to_holder(opts) {
                return Err(held);
            }
            match self.reconcile(review) {
                Ok(()) | Err(Fail::Lost(_)) => {}
                Err(Fail::Hard(e)) => return Err(e),
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
            match self.look(opts, &allow, &mut state, &mut deadline, review) {
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
    /// the loop. First a possible stray `1` left in the composer
    /// ([`Self::stray`]) is backspaced and noted. Then the session survey
    /// ([`Self::survey`]) is said, or dismissed under `dismiss_surveys`. The
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
    fn look(
        &mut self,
        opts: &SuperviseOpts,
        allow: &[String],
        state: &mut Looking,
        deadline: &mut Instant,
        review: &mut dyn Review,
    ) -> Result<Option<End>, Fail> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        // A screen the hold read past its point still showing a point is
        // this look's turn; one busy again is awaited as any turn is.
        let carried = state.carried.take().filter(|s| {
            let reader = self.reader(&s.rows);
            reader.phase(&s.rows) != Phase::Busy && reader.composer(&s.rows).is_some()
        });
        // A press's read handed on is this look's turn as read.
        let read_back = state.read_back.take();
        let turn = match carried.or(read_back) {
            Some(screen) => self.turn_of(screen),
            None => {
                // The rating card is also dealt with on the BUSY reads of the wait: a
                // worker that reads busy for an hour ([`SurveyDuty`]). Not by a loop
                // whose word is Off: no card is touched under Off (the design's
                // §2.2), and the look parks at the turn's point.
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
        // A `/usage` panel of the harness's that is owed its close goes
        // FIRST, before anything of this look is decided, pressed or booked
        // — before what the reads saw is taken, so a live turn read on the
        // way still counts at the point that follows: no decision is ever
        // made on a panel screen ([`Self::usage_panel_look`]). Nothing is
        // owed, and nothing read or pressed here, on any loop that never
        // typed a probe and whose ledger holds none. The budget bounds it as
        // it bounds every act: a turn read at or after the deadline is the
        // TIMEOUT below. Ahead of Autopilot Off's park too: the panel is the
        // harness's own, and an Off session is left as plain Claude Code,
        // never with the harness's `/usage` standing on it — but under Off it
        // gets one plain close and no more (no stall's hold, no retry, and
        // no wait but a ledger panel's grace, which presses nothing: the
        // park instead, the panel owed until the word turns).
        let off = self.autopilot(opts) == Autopilot::Off;
        if review.unattended()
            && !turn.timed_out
            && Instant::now() < *deadline
            && self.usage_panel_look(&turn, state, *deadline, off, opts, review)?
        {
            state.gone_first = false;
            return Ok(None);
        }
        // AUTOPILOT OFF, by the word the loop last read (the start-up
        // reconcile's, a decision's): hands off before anything else of this
        // look is decided, pressed, booked or reported — a limit reset's
        // step included, which may key the purchase's confirmation and waits
        // for the word to change (the design's §7) — parked until it does
        // ([`Self::park_while_off`]), then read afresh.
        if review.unattended() && !turn.timed_out && Instant::now() < *deadline && off {
            self.park_while_off(turn.screen.seq, *deadline, opts, review)?;
            state.gone_first = false;
            return Ok(None);
        }
        // A limit reset's step that is owed goes next, as the panel does and
        // for the same reason: the pending line and the confirmation the
        // harness's own row opened are never decided by a rule of the
        // approval policy ([`Self::reset_look`]). Nothing is owed, and
        // nothing read or keyed here, on any loop that took no grant row and
        // whose ledger holds none.
        if review.unattended()
            && !turn.timed_out
            && Instant::now() < *deadline
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
        // A read since the last point saw the worker busy: it worked, and the
        // point showing now is a new one — save the box handed over, still
        // showing, when no read since showed the worker's OWN busy row, every
        // busy read being only output written (`writing`: [`Saw::live`]).
        // That box was redrawn through the writing (the box off the screen a
        // frame, a transcript reflowing over it), not asked again after work:
        // it is passed over as any redraw of it is, and not asked again.
        let saw = std::mem::take(&mut state.moved);
        let worked = saw.busy;
        // The accounts probe goes only at the point of a turn read LIVE
        // ([`Self::turn_live`]): this look's, never a later look at the same
        // point.
        self.turn_live = saw.live;
        // …and a turn since the last point makes this one NEW to the probe's
        // one line a point, though its words be the last one's
        // ([`Self::probe_shut`]): the point printed again has its line again.
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
            self.question_done(turn.screen.seq, opts.notes.as_deref(), review)?;
        } else if !matches!(turn.phase, Phase::Prompt | Phase::Busy) {
            // No read saw it busy, and yet the worker sits at its composer
            // with no dialog up: the dialog in hand is done, however it ended
            // (a dialog between its tabs, or at its review, is still a box:
            // `Phase::Prompt`). One the loop answered in a reply short enough
            // to start and end inside the settle after its Enter is told
            // here. One that went with no answer of the loop's — a person's
            // Esc after the loop's text — is forgotten here, its text among
            // it: remembered, the same question asked again would read as
            // the text "gone" from a new dialog's empty field, and be handed
            // over (ATERM-KI-1015-1 F4). A memory lost early is a hand-over,
            // never an answer.
            self.question_done(turn.screen.seq, opts.notes.as_deref(), review)?;
        }
        if worked {
            // A badge the turn-end policy raised with no point to carry it
            // (`/login` typed): the worker works again, so it is done — but
            // the note that Codex's goal runs on after every stop, which is
            // kept up while it does ([`Self::goal_badge`]).
            if std::mem::take(&mut self.turn_end_badge) {
                let said = self.unset_if_ours(|t| !t.contains(GOAL_NOTE_HEAD))?;
                review.note(&format!(
                    "CLEARED seq={} {CLEARED_TURN_END} attention={said}",
                    turn.screen.seq
                ));
            }
        }
        // The goal's note goes once the switch has closed, or the footer
        // shows Codex's goal no longer pursued.
        if self.goal_badge
            && (!self.turn_end.switch_open()
                || self.codex_goal != Some(aterm_phase::codex::CodexGoal::Pursuing))
        {
            self.goal_badge = false;
            let said = self.unset_if_ours(|t| t.contains(GOAL_NOTE_HEAD))?;
            review.note(&format!(
                "CLEARED seq={} {CLEARED_GOAL} attention={said}",
                turn.screen.seq
            ));
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
        if turn.timed_out || Instant::now() >= *deadline {
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
        // The worker answered on a point that is not the notice: an episode
        // a busy read closed is over for good ([`Retry`]), and so is a limit
        // whose badge was adopted from a previous loop
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
        if let Some(command) = self.stray.take()
            && stray_digit(self.reader(&turn.screen.rows), &turn.screen, "1")
        {
            // A person typing: the `1` is left for the look after their
            // grace, and this look goes on as any other.
            if self.held_for_person(turn.screen.seq, "a stray 1's backspace", true, opts, review)? {
                self.stray = Some(command);
            } else {
                match self.backspace() {
                    Ok(Backspaced::Done) => {}
                    // Nothing was deleted: the `1` still stands and its removal is owed,
                    // so the marker stays and the loop parks or backs off, then looks again.
                    Ok(Backspaced::Refused { halted, why }) => {
                        self.stray = Some(command);
                        self.refused(halted, &why, turn.screen.seq, review)?;
                        state.gone_first = false;
                        return Ok(None);
                    }
                    Err(e) => {
                        self.stray = Some(command);
                        return Err(e);
                    }
                }
                append_note(
                    opts.notes.as_deref(),
                    &format!(
                        "backspaced a 1 left in the composer by the press for: {command} (the \
                         connection was lost before the press was checked)"
                    ),
                )?;
                state.gone_first = false;
                return Ok(None);
            }
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
            }) {
            // The point already handed over, still showing: the screen
            // moved (a footer tick, a banner) and nothing the point is made
            // of did. Nothing is pressed or reported.
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
            if Instant::now() >= *deadline {
                return Ok(Some(End::Timeout(point)));
            }
            // The turn-end policy's decision, made once when the point is new
            // ([`Self::turn_end_at_point`]): the turn's work is the time since
            // the first busy read after the last point (or the nudge's box,
            // [`RunningTurn::point`]); a latched person's hand is spent.
            let busy_for = self.running.point(Instant::now());
            let turn_end = if review.unattended() {
                self.turn_end_at_point(&point, busy_for, opts)?
            } else {
                TurnEndAction::Nothing
            };
            // AUTOPILOT OFF, read for this point's decision (which then
            // decided nothing): the point is neither reported, escalated nor
            // acted on — a limit's episode, the host's step and the probe
            // included — and the loop parks until the word changes, then
            // reads and decides the screen afresh ([`Self::park_while_off`]).
            if review.unattended() && self.autopilot(opts) == Autopilot::Off {
                self.park_while_off(point.screen.seq, *deadline, opts, review)?;
                state.gone_first = false;
                return Ok(None);
            }
            // A limit whose wall the transcript refused is an idle point, and
            // a limit a cancel left that answered a turn is a limit point
            // ([`Self::point_as_decided`]): its episode, its escalation.
            self.point_as_decided(&mut point);
            // `--mail`: an idle point waits for the worker's report of this
            // turn first; a folded one reads no report from the screen (the
            // mail IS what was said).
            let Held { fold, carried } = if point.phase == Phase::Idle {
                self.hold_for_report(&point, *deadline)?
            } else {
                Held {
                    fold: None,
                    carried: None,
                }
            };
            let report = if opts.report
                && reported(&point.phase)
                && !matches!(fold, Some(Fold::Report { .. }))
            {
                Some(self.brief_report(opts)?)
            } else {
                None
            };
            // A box the loop did not approve, a question the turn-end policy
            // does not take as an offer, and a point the policy escalates are
            // the manager's: escalated once ([`Self::escalate_point`];
            // `supervise` does none of it). A limit's escalation is its
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
                        self.escalate_point(&point, None, review)?;
                    }
                    (_, TurnEndAction::Escalate { reason }) if point_escalated => {
                        self.escalate_point(&point, Some(reason), review)?;
                        if self.memory_wall {
                            self.mark_memory_badge();
                        }
                    }
                    _ => {}
                }
            }
            if !review.review(&point, report, fold)? {
                return Ok(Some(End::Stopped(point, fold)));
            }
            // The manager has the worker now, as after a fresh `supervise`.
            state.approved.clear();
            state.handed = Some(self.review_key(&point));
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
            // `--mail`: a turn ended here; the next begins after it.
            if let Some(m) = &mut self.mail
                && reported(&point.phase)
            {
                m.floor = Instant::now();
                m.busy_at = None;
                m.handed = true;
            }
            if carried.is_some() {
                // The screen has moved past the point already, and was read:
                // what the policy decided on it is not done on another.
                state.carried = carried;
                state.gone_first = false;
                return Ok(None);
            }
            // The host's step goes first at an idle point it asked for (the
            // live upgrade, a relaunched agent's continuation): the point's
            // own act is not taken on a screen the step has moved.
            if self.host_steps_here(&point, opts, review)? {
                // The host took the point: a probe the roster said was due
                // here is not run, and says so.
                self.probe_shut(&point, ProbeGate::HostWants, None, review)?;
                state.gone_first = false;
                return Ok(None);
            }
            if review.unattended() {
                // THE ACCOUNTS PROBE, where the host claims one for this
                // point ([`Self::usage_probe`]): a step of the loop's own,
                // between the host's step and the point's act, that moves
                // none of the policy's memory. With no claim — every point
                // of every session on a machine with no roster — it is `No`
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

    /// The session survey at one look ([`super::phase::survey_open`]). When it
    /// appears (open now, not said since a read last showed it gone,
    /// [`Survey::Closed`]) the loop says `EVENT survey seq=<n> dismiss with:
    /// aterm ctl @<sid> key 'if=^●.How.is.Claude.doing' 0`
    /// ([`survey_event_line`]), once per appearance ([`Session::survey_gone`]).
    ///
    /// Under `dismiss_surveys` the loop presses that `0` itself
    /// ([`Self::dismiss_survey`]) and `true` has it look again from a fresh
    /// read, where the `0` is judged ([`Survey::Tried`]): the survey gone,
    /// the loop says `DISMISSED survey seq=<n>` (the press's stamp; noted in
    /// `--notes` too), or nothing when the press was skipped; the survey
    /// still open — the `0` did not take, or the guard matched no row of it —
    /// is TRIED AGAIN on a growing back-off ([`SURVEY_RETRY`],
    /// [`Self::retry_survey`]): the `EVENT survey` line said at the first
    /// failure, the host told from the [`SURVEY_BADGE_AT`]-th (a record; it ends
    /// with the survey), the tries going on; a `0`
    /// that landed in the composer instead is backspaced and noted, nothing
    /// is said dismissed, and a survey still open then is tried again the
    /// same way. The survey gone over text a person typed that BEGINS with the
    /// `0` (`0hello`, `00`: their key landed in the ~0.44 s the card takes to
    /// take the digit, which abandons the card) was not dismissed: the journal
    /// says `ABANDONED seq=<n> …` and `--notes` the same, and the text is theirs
    /// — the `0` is not taken out of it. A person who keyed between the read
    /// and the press skips the `0` altogether (`if-human=`, [`Dismiss::Yielded`]):
    /// no try is spent, and their grace holds the card at the next look.
    ///
    /// While a box is up or a draft is typed, the survey waits (a `0` would
    /// land in the draft). Within a person's grace ([`Self::held_for_person`])
    /// neither the `0` nor a stray's backspace goes. A press with no answer is
    /// ridden out with the survey not yet tried.
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
                .is_some_and(|at| at.elapsed() >= self.survey_land)
        {
            self.survey_zero = None;
        }
        // THE `0` IS WAITING ON THE CARD. Claude Code takes a digit that stands alone in the
        // composer 400 ms after it appeared, and ANY change to the composer in that time
        // cancels the take: so while the card is open, a `0` of ours younger than
        // [`SURVEY_LAND`] is neither judged nor removed — a backspace here would delete our
        // own pending dismissal (measured on a stand-in with a 500 ms debounce: the
        // backspace came at 510 ms and the card was never dismissed, try after try). The
        // wait is the card leaving; the loop looks again from a fresh read.
        if let (Some(block), Some(pressed)) = (&block, self.survey_zero) {
            let wait = self.survey_land.saturating_sub(pressed.elapsed());
            if !wait.is_zero() {
                self.survey_gone |= left;
                self.survey_pending(block, turn.screen.seq, wait)?;
                return Ok(true);
            }
        }
        // A `0` of ours standing alone in the composer past that time was dropped by the card
        // (it had mounted again, or the hook was not armed): backspaced, whatever state the
        // machine is in — never left for the next message to begin with. Only an `OK` removes
        // it: a refusal (`halted`, `busy`, `rate`) parks or backs the loop off and keeps the
        // `0` owed, so the removal is tried again, not forgotten.
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
            let why = if open {
                "the survey, still open, did not take it"
            } else {
                "it had left first"
            };
            append_note(
                opts.notes.as_deref(),
                &format!(
                    "backspaced a 0 left in the composer by the press for the session survey \
                     ({why}; nothing was dismissed)"
                ),
            )?;
            if let Survey::Tried { tries, .. } = *survey {
                *survey = Survey::Closed;
                if open {
                    self.retry_survey(turn, &ctx, survey, review, tries)?;
                }
            }
            return Ok(true);
        }
        // A draft is judged where it is DRAWN ([`Self::composer_draft`]: the
        // cursor's column and the cell's dim, never its row), as the act of a
        // turn end judges it: a person's draft whose caret was moved home sits at
        // column 2 like the placeholder, and a `0` pressed in front of it is
        // theirs to find in their text.
        let composer = reader.composer(&turn.screen.rows);
        let draft = turn.phase != Phase::Prompt
            && self.composer_draft(composer.as_ref(), reader.placeholder(), &turn.screen);
        let waits = turn.phase == Phase::Prompt || draft;
        let mut tries = 0;
        match *survey {
            Survey::Tried { pressed, .. } if left => {
                self.survey_zero = None;
                let kind = self.survey_kind.take();
                match pressed {
                    // A person's key landed in the ~0.44 s the card spends taking the `0`
                    // (measured on Claude Code 2.1.286: 13 of 13 runs): the card is
                    // ABANDONED by it, not dismissed, and the `0` heads what they typed
                    // (`0hello`, `00`). Said as it is; their text is theirs, and the `0`
                    // stays in it.
                    Some(seq) if draft && heads_with_zero(composer.as_ref()) => {
                        append_note(
                            opts.notes.as_deref(),
                            "the session survey left under text typed in the composer (the 0 \
                             pressed for it may head that text; nothing was dismissed)",
                        )?;
                        review.note(&format!(
                            "ABANDONED seq={seq} the session survey left under typed text; a 0 of \
                             ours may head it"
                        ));
                    }
                    Some(seq) => {
                        append_note(
                            opts.notes.as_deref(),
                            "dismissed the session survey (pressed 0; a rating is the human's)",
                        )?;
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
            Survey::Tried { pressed, tries } => {
                let why = if pressed.is_some() {
                    "still open after its guarded 0"
                } else {
                    "its guarded 0 matched no row, and it is still open"
                };
                append_note(
                    opts.notes.as_deref(),
                    &format!("the session survey was not dismissed ({why}); tried again"),
                )?;
                self.retry_survey(turn, &ctx, survey, review, tries)?;
                return Ok(false);
            }
            Survey::Retry { .. } if left => {
                *survey = Survey::Closed;
                self.survey_ended(turn.screen.seq, review);
            }
            Survey::Retry { at, .. } if Instant::now() < at => {
                // The idle look re-decides at `at`; a busy read has no point to look at
                // again, and its next read comes with the screen.
                if !busy {
                    self.look_at = Some(self.look_at.map_or(at, |t| t.min(at)));
                }
                return Ok(false);
            }
            // A busy read presses a card ONCE per appearance. Its premise — that Claude Code's
            // digit hook takes a digit while the worker reads busy — is read from the vendor's
            // code (`zu` enables itself on the card's own mount, not on the turn) and was
            // observed on 2.1.286 (over a streaming turn, a background shell, a woken turn); so
            // a `0` that did not take is followed by no second one from a busy read: the idle
            // look at the turn's end carries the ladder (5 s, 30 s, 2 min, 10 min), and a hook
            // that is not armed under load costs one `0` and its backspace, not a cycle for as
            // long as a workflow runs.
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
                    self.survey_zero = Some(Instant::now());
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
                Dismiss::Unguarded => {}
            }
        }
        *survey = Survey::Said;
        review.say(&survey_event_line(
            turn.screen.seq,
            self.sid.as_deref(),
            &block,
        ))?;
        // A card the harness will not press (a shape nobody has words for, or a question row no
        // guard can fence) is not answered by anyone unless someone is told: nothing else
        // acts at a point with a survey open, so a worker nobody watches would stand there
        // silently. The badge goes with the point, as the ladder's does.
        if let Some(why) = withheld
            && !busy
        {
            self.tell_withheld_survey(turn, why, review)?;
        }
        Ok(false)
    }

    /// A rating card the harness will not press is open: told to the host once per
    /// appearance, for a reader nobody is attending; it ends with the card
    /// ([`Self::survey_ended`]). No tab is marked (owner rule, 2026-09-28: a badge
    /// says the owner is needed NOW, and this card blocks nothing the worker does).
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
    /// question row — and a host without `await gone` waits for the screen's next move.
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
    /// and, on a host without `await gone` (or with no guard to name), the
    /// screen's next move past `seq`. The rating card's own question row
    /// ([`Self::survey_pending`]) and the Settings panel's header row
    /// ([`Self::usage_panel_look`], [`Self::panel_escape`]) are waited on so:
    /// a person who closes either ends the wait at once.
    pub(super) fn wait_row_gone(
        &mut self,
        guard: &str,
        seq: u64,
        wait: Duration,
    ) -> Result<(), Fail> {
        if self.caps.gone != Some(false) && !guard.is_empty() {
            match self.wait(&["gone", guard], wait)? {
                Wait::Unsupported => self.caps.gone = Some(false),
                _ => {
                    self.caps.gone = Some(true);
                    return Ok(());
                }
            }
        }
        self.wait(&["seq", &seq.to_string()], wait)?;
        Ok(())
    }

    /// How much longer the rating card now open must show before its `0` is pressed
    /// ([`SURVEY_SETTLE`]), counted from the read that first showed it — `None` once
    /// it has shown that long. The card is remembered until a read shows it gone
    /// ([`Self::screen`]), so a card held for a person waits no more.
    fn survey_young(&mut self) -> Option<Duration> {
        let first = *self.survey_first_seen.get_or_insert_with(Instant::now);
        let wait = self.survey_settle.saturating_sub(first.elapsed());
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

    /// [`Self::held_for_person`] for the reads of a BUSY wait: the same test (a person
    /// typed within `human_grace_s` of a status read made now), but no `turn_end_due`
    /// and no line per read — the hold is remembered until the grace ends, and noted
    /// once. The idle look's hold re-arms the loop's wait ([`Self::turn_end_due`]); a
    /// busy read has no point to look at again, and its next read comes with the screen.
    fn survey_held_busy(
        &mut self,
        seq: u64,
        act: &str,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if self.survey_hold.is_some_and(|until| until > Instant::now()) {
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

    /// The journal's line for an act held for a hand on the session until
    /// `until` — `HELD seq=<n> <who>: hands off until <UTC> (<act>)`, the
    /// words [`Self::held_for_person`] writes — for a hold that is not a
    /// look's own: the survey's `0` on a busy read
    /// ([`Self::survey_held_busy`]) and the accounts probe's Escape
    /// ([`Self::usage_panel_look`]), each said once a hold.
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
                    + until.saturating_duration_since(Instant::now()).as_secs()
            )
        )
    }

    /// The survey's `0` did not dismiss it (`failed` tries before this
    /// one): tried again after [`SURVEY_RETRY`]'s pause, the loop looking
    /// again then ([`Self::look_at`]). The first failure says the `EVENT
    /// survey` line (a watching manager's cue); from the
    /// [`SURVEY_BADGE_AT`]-th the host is told ([`IdleHost::survey_stuck`]: a
    /// record, and a line of the digest after an hour; never a tab mark)
    /// while the tries go on — never handed over for good.
    fn retry_survey(
        &mut self,
        turn: &Turn,
        ctx: &SurveyCtx<'_>,
        survey: &mut Survey,
        review: &mut dyn Review,
        failed: u32,
    ) -> Result<(), Fail> {
        let tries = failed.saturating_add(1);
        let pause = super::ladder::Ladder(&self.survey_retry)
            .step(usize::try_from(tries - 1).unwrap_or(usize::MAX));
        let at = Instant::now() + pause;
        *survey = Survey::Retry { tries, at };
        // The idle look re-decides at `at`; a busy read has no point to look at again (its
        // next read comes with the screen, and finds the retry due).
        if !ctx.busy {
            self.look_at = Some(self.look_at.map_or(at, |t| t.min(at)));
        }
        if tries == 1
            && let Some(block) = ctx.block
        {
            review.say(&survey_event_line(
                turn.screen.seq,
                self.sid.as_deref(),
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

    /// `watch --report`'s count of the report at a point. An unserved request
    /// is ridden out; any other report failure does not end the loop: the point
    /// is reported `complete=0 rows=0` and the failure noted.
    fn brief_report(&mut self, opts: &SuperviseOpts) -> Result<ReportBrief, Fail> {
        match self.gather_report(&ReportOpts::default()) {
            Ok(r) => Ok(ReportBrief {
                complete: r.complete(),
                rows: r.rows.len(),
                turn: r.turn,
            }),
            Err(Fail::Lost(why)) => Err(Fail::Lost(why)),
            Err(Fail::Hard(why)) => {
                append_note(
                    opts.notes.as_deref(),
                    &format!("report failed: {}", one_line(&why)),
                )?;
                Ok(ReportBrief {
                    complete: false,
                    rows: 0,
                    turn: None,
                })
            }
        }
    }

    /// `--mail` at an idle point: how it is settled ([`Fold`]), or no fold
    /// when there is no lane. The newest pending `report` that is this turn's
    /// ([`MailIn::is_this_turns`]) folds at once (older ones are dropped); with
    /// none pending the point is held until one lands or the grace runs out
    /// ([`Fold::NoReport`]), bounded by the budget like every wait.
    ///
    /// THE SCREEN IS THE SAFETY NET UNDER THE HOLD: once a `hold_step` (20 s)
    /// of it has passed with no report, the screen is read once — a footer that
    /// ticked is the same point ([`Self::review_key`]) and the hold goes on; a
    /// prompt, a question or the worker busy again has superseded the point,
    /// which is said as `idle-no-report` (the turn did end, and no report came
    /// before the worker moved on) with the screen read carried to the next
    /// look ([`Held::carried`]). Without it a prompt right after an idle — a
    /// queued message, a wake — waited the whole grace unseen.
    fn hold_for_report(&mut self, point: &Turn, deadline: Instant) -> Result<Held, Fail> {
        let none = Held {
            fold: None,
            carried: None,
        };
        let Some(m) = self.mail.as_mut() else {
            return Ok(none);
        };
        loop {
            match m.rx.try_recv() {
                Ok(d) => m.pending.push(d),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    m.gone = true;
                    break;
                }
            }
        }
        let now = Instant::now();
        let recent = m
            .pending
            .iter()
            .filter(|d| m.is_this_turns(d, now))
            .max_by_key(|d| d.id)
            .map(|d| Fold::Report {
                id: d.id,
                rows: d.rows,
            });
        m.pending.clear();
        if let Some(fold) = recent {
            return Ok(Held {
                fold: Some(fold),
                carried: None,
            });
        }
        if m.gone {
            return Ok(none);
        }
        let until = now
            .checked_add(m.grace)
            .map_or(deadline, |t| t.min(deadline));
        let key = self.review_key(point);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(Held {
                    fold: Some(Fold::NoReport),
                    carried: None,
                });
            }
            let slice = left.min(self.hold_step);
            let got = match self.mail.as_mut() {
                Some(m) => m.rx.recv_timeout(slice),
                None => return Ok(none),
            };
            match got {
                Ok(d) => {
                    return Ok(Held {
                        fold: Some(Fold::Report {
                            id: d.id,
                            rows: d.rows,
                        }),
                        carried: None,
                    });
                }
                Err(RecvTimeoutError::Disconnected) => {
                    if let Some(m) = self.mail.as_mut() {
                        m.gone = true;
                    }
                    return Ok(none);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if slice == left {
                        // The grace ran out with this slice: nothing to look
                        // at that the next look will not read anyway.
                        continue;
                    }
                    let screen = self.screen()?;
                    let now_turn = self.turn_of(screen);
                    if self.review_key(&now_turn) == key {
                        continue;
                    }
                    return Ok(Held {
                        fold: Some(Fold::NoReport),
                        carried: Some(now_turn.screen),
                    });
                }
            }
        }
    }

    /// Ride out an outage: a request the server did not serve
    /// ([`Self::unserved`]), as when a self-update hands every session to a new
    /// instance under the same `@sid`. Reads the screen of the same selector
    /// after a pause starting at 0.5 s and doubling up to 8 s across the
    /// outage, until a read answers (`Rode::Back`) or the window runs out. The
    /// client resolves its socket afresh each request: unnamed, that is the
    /// instance hosting the caller's terminal, else the newest (`aterm.sock`);
    /// a named per-instance socket (`aterm-<pid>.sock`) goes with its instance.
    ///
    /// The window is the outage's, from the first unserved request until the
    /// loop gets past it (the failed kind served again, an `await seq`
    /// latching, or a look getting through). A probe answering only says the
    /// session answers a read, so a later unserved request is the same outage.
    /// `RECONNECT <why>` (cut at 160 characters) is said at the first ride-out
    /// and `RECONNECTED after <ms> ms` at the first answer.
    ///
    /// Ends the loop (`Err`) when the window runs out (`reconnect window
    /// lapsed: <the last failure>`), at once with `why` when the window is zero
    /// (`--reconnect-s 0`), and on a probe `ERR` that [`Self::unserved`] does
    /// not name. Budget spent first is `Rode::Spent`, the loop's TIMEOUT. The
    /// probe is a read, never the failed request: a new instance's content seq
    /// starts over, and a press is decided again from a fresh read. Probed caps
    /// are forgotten first, since the answering instance may be another build.
    fn ride_out(
        &mut self,
        why: String,
        deadline: Instant,
        say: &mut dyn FnMut(&str) -> Result<(), String>,
    ) -> Result<Rode, String> {
        let now = Instant::now();
        if now >= deadline {
            return Ok(Rode::Spent);
        }
        if self.reconnect.is_zero() {
            return Err(why);
        }
        let first_pause = self.pause;
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
            .checked_add(self.reconnect)
            .map_or(deadline, |end| end.min(deadline));
        self.caps = Caps::default();
        let mut last = why;
        loop {
            if self.stopped() {
                return Ok(Rode::Stopped);
            }
            let now = Instant::now();
            if now >= end {
                return if now >= deadline {
                    Ok(Rode::Spent)
                } else {
                    Err(format!("reconnect window lapsed: {last}"))
                };
            }
            let pause = self.outage.as_ref().map_or(first_pause, |o| o.pause);
            std::thread::sleep(pause.min(end - now));
            if let Some(o) = self.outage.as_mut() {
                o.pause = pause.saturating_mul(2).min(self.pause_max);
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
                        let ms = since.elapsed().as_millis();
                        say(&format!("RECONNECTED after {ms} ms"))?;
                    }
                    return Ok(Rode::Back { fresh });
                }
                Err(Fail::Lost(e)) => last = e,
                Err(Fail::Hard(e)) => return Err(e),
            }
        }
    }

    /// Wait (one step, capped by `deadline`) for the content to move past
    /// `seq`. A step that runs out is checked with a read: `await seq <n>`
    /// latches only once THIS instance's count passes `n`, so after a handoff
    /// no request saw fail — the instance handing over answered the step (its
    /// readers parked, its count frozen), or the step ended as the successor
    /// took the socket — the loop would wait on a count the successor, whose
    /// count started over, may not reach for hours. A read BELOW `seq` is
    /// that other count: the screen moved. Nothing is read once the budget is
    /// spent. A step is `step_cap` at most.
    fn moved_past(
        &mut self,
        seq: u64,
        deadline: Instant,
        step_cap: Duration,
    ) -> Result<Past, Fail> {
        let step = deadline
            .saturating_duration_since(Instant::now())
            .min(step_cap);
        match self.wait(&["seq", &seq.to_string()], step)? {
            Wait::Latched => Ok(Past::Moved),
            Wait::TimedOut if Instant::now() >= deadline => Ok(Past::Still(None)),
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

/// The limit episode: `watch`'s alone (see the module doc). Every request
/// here goes through [`Session::call`] like the loop's own, so an outage
/// under any of them is ridden out the same way.
impl<C: Ctl> Session<'_, C> {
    /// Unix seconds now: the fixed clock in tests, else the system's.
    fn now_unix(&self) -> i64 {
        #[cfg(test)]
        if self.clock.is_some() {
            return self.now_unix_ms().div_euclid(1000);
        }
        self.clock.unwrap_or_else(limit::unix_now)
    }

    /// Unix MILLISECONDS now: the clock the loop turns a unix time into an
    /// `Instant` by, and back ([`Self::instant_of`], [`Self::unix_of`]). The
    /// fixed clock in tests (whole seconds, plus the phase
    /// [`Self::set_clock_at_ms`] gives it), else the system's.
    ///
    /// NOT WHOLE SECONDS: converted against [`Self::now_unix`], which
    /// truncates, a reset placed at `U` would become an `Instant` carrying the
    /// sub-second phase of the read that made it while its journal stamp drops
    /// the phase of the read that prints it; whenever a second turns between
    /// the two, one placement is journaled as two `until=`s a second apart.
    fn now_unix_ms(&self) -> i64 {
        #[cfg(test)]
        if let Some(fixed) = self.clock {
            use std::sync::atomic::Ordering::SeqCst;
            let jump = self.clock_jump.load(SeqCst);
            let run = self.clock_runs_from.map_or(0, |from| {
                i64::try_from(from.elapsed().as_millis()).unwrap_or(0)
            });
            let secs = fixed + self.clock_skew.swap(jump, SeqCst);
            return secs * 1000 + self.clock_phase_ms + run;
        }
        self.clock
            .map_or_else(limit::unix_now_ms, |secs| secs.saturating_mul(1000))
    }

    /// The local zone's offset, read from `date` once.
    fn local_offset_s(&mut self) -> i64 {
        if let Some(o) = self.local_offset {
            return o;
        }
        let o = limit::tz_offset_s();
        self.local_offset = Some(o);
        o
    }

    /// The reset a notice names as Unix seconds; `None` when it names none the
    /// watcher can read. The reset the loop PLACED for this wall
    /// ([`Self::wall_reset_at`]: [`Self::reset_unix_for`], by the wall's kind and
    /// print) when it placed one for these words — the ONE placement, so the
    /// budget stretched past the reset (`EXTEND`) and the wait for it agree —
    /// else the host's where the words are its agent's to place
    /// ([`IdleHost::place_reset`]: a Codex's, in its own zone), else
    /// [`limit::reset_at`]'s, the session window's rule, for a bare clock time
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
        if let Some(placed) = self
            .stall_host
            .as_ref()
            .and_then(|h| h.place_reset(text, now))
        {
            return placed;
        }
        let spec: ResetSpec = limit::parse_reset(text)?;
        let local = self.local_offset_s();
        let at = limit::reset_at_with(&spec, now, local, &self.zone_offset);
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

    /// [`Self::reset_unix`] for a wall of `kind`.
    ///
    /// The host places the words first where they are its agent's to place
    /// ([`IdleHost::place_reset`]: a Codex's, in its own zone): its placement
    /// is final. A Codex names its date otherwise, so a bare clock time of its
    /// is the look's day's, ahead or passed — [`limit::reset_at`]'s — and the
    /// print's rule below is Claude Code's alone.
    ///
    /// A session window (`session_window`: the notice says `session limit` or
    /// `5-hour`; a `Usage limit reached · …` that names no window may be a
    /// weekly one) resets within five hours, so a bare clock time on one is
    /// [`limit::reset_at`]'s: today's, or tomorrow's only when that is
    /// under five hours off. Every other wall's bare clock time is the vendor's
    /// own doing: its formatter prints one only for a reset within 24 hours of
    /// the print and the date beyond that (`nu(resetsAt, …)` in the 2.1.284
    /// binary: `m > 24 ? month day hour : hour`), so the reset is the FIRST
    /// OCCURRENCE OF THAT CLOCK AFTER THE PRINT ([`limit::place_reset`]), and
    /// the print is what the screen says of it: the done row's clock (`done
    /// 12:19 PM`, Claude Code's own stamp of when the turn ended on the wall)
    /// — its last occurrence within a day, at the start of its minute — else,
    /// for a wall the loop saw ARRIVE (`live`), [`PRINT_LAG_S`] ago, less
    /// [`CLOCK_SLACK_S`] either way. A wall
    /// FOUND sitting with no done row (a relaunch redraws the conversation
    /// without one) has no evidence of its print: the session rule places it
    /// (`None`), and a wrong guess costs one try — the wall answers it,
    /// reprinted after a turn or standing with the line held and no turn, and
    /// either is a print the loop sees ([`Self::wall_reset_at`],
    /// [`TurnEndState::untaken_act`]).
    ///
    /// Either way a bare clock time is then read against the agent's
    /// transcript ([`Self::transcript_day`]): where it names that same clock
    /// time on an earlier day, that is the reset.
    ///
    /// `answered`: the wall stands where an act of the loop's went untaken,
    /// inside its take window or past it ([`TurnEndState::untaken_act`]) — it
    /// answered that act,
    /// so the limit stood at this read and the print is no earlier than
    /// [`PRINT_LAG_S`] ago, whatever done row stands above the composer: a
    /// long turn's row of hours before still stands when the act drew none of
    /// its own (Claude Code held the line, no turn ran), and read alone it
    /// puts a weekly `5am` seen at 17:26 after a `done 3:21 AM` in the past.
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
        if let Some(placed) = self
            .stall_host
            .as_ref()
            .and_then(|h| h.place_reset(text, now))
        {
            return placed;
        }
        let spec: ResetSpec = limit::parse_reset(text)?;
        let local = self.local_offset_s();
        let at = if places_resets_on_print(self.reader(rows).program()) {
            let printed = limit::printed_by_the_screen(rows, now, local, live)
                .max(answered.then_some(now - limit::PRINT_LAG_S - limit::CLOCK_SLACK_S));
            let zone = &self.zone_offset;
            limit::place_reset_with(&spec, session_window, now, printed, local, zone)
        } else {
            limit::reset_at_with(&spec, now, local, &self.zone_offset)
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

    /// `--mail`: a turn the watcher sent (a continuation, a command) begins
    /// a turn as a handed-over point does — its report is not the last
    /// point's.
    fn mail_turn_boundary(&mut self) {
        if let Some(m) = &mut self.mail {
            m.floor = Instant::now();
            m.busy_at = None;
            m.handed = true;
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
                    let text = format!("{ATTENTION_PREFIX} {}", one_line(message));
                    self.escalate(point.screen.seq, &text, None, "control", review)?;
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
    /// reopened: attention set again, no second mail, its seq carried, the
    /// reset the notice names now.
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
        if handled {
            review.note(&format!("LIMITED seq={seq} handled: {}", clip(message)));
        } else {
            let text = format!("{ATTENTION_PREFIX} {}", one_line(message));
            self.escalate(seq, &text, retry.as_ref().map(|r| r.seq), "control", review)?;
        }
        self.limit = Some(Episode {
            seq: retry.map_or(seq, |r| r.seq),
            resumes_itself: limit::resumes_by_itself(message),
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

    /// A busy read inside the wait for a turn ([`Self::await_turn_from`]) with
    /// an episode open whose notice said Claude Code would go on by itself
    /// ([`Episode::resumes_itself`]): the episode closes now (`CLEARED seq=<n>
    /// … the worker works again`, attention unset), not at the next review
    /// point, which the resumed turn may keep hours off; the notice row still
    /// on screen changes nothing. An episode whose notice named a reset is
    /// untouched. The closed episode is kept ([`Retry`]) until the worker
    /// answers on a point that is not the notice, so a retry that hits the wall
    /// again reopens the same episode with no second mail
    /// ([`Self::open_episode`]).
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
            Instant::now() + Duration::from_secs(u64::try_from(until_unix - now_unix).unwrap_or(0));
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

    /// The wait for the screen to move past `seen`, step after step until the
    /// budget, bounded by the turn-end policy's `WaitUntil`
    /// ([`Self::turn_end_due`]), when the policy decides again on the point
    /// still showing ([`Self::turn_end_now`]). A screen change ends the wait
    /// first. `Some(End::Timeout)` when the budget runs out; `None` otherwise,
    /// including after the host's step at an idle point
    /// ([`Self::host_steps_here`]).
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
    /// footer must be read and the content is waited on; anything else, the
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
            next_wait_step()
        };
        // A Mac that slept through the wait. `Instant` does not count sleep
        // and the wall clock does: the policy's deadline falls at its wall
        // time all the same ([`Self::due_at`]), and when the wall clock has
        // run ahead of the monotonic one by more than a minute — between two
        // reads of the session's watch, whatever point shows — a deadline the
        // sleep carried past is decided again after the wake's settle, the
        // sessions of one account spread ([`Self::check_slept`],
        // [`Self::woke`]). The clocks are read on entering the wait too.
        self.watch_sleep();
        // The review key of `seen`, built at most once for the whole wait
        // ([`Self::agent_wake`]).
        let mut seen_key: Option<String> = None;
        // What the FILE box `seen` shows asks, for the read after its first
        // row stood a whole step: `None` — and no such read — for every other
        // point, a question held for the person on its short steps included
        // (a question is no file box; `held` says so twice).
        let request = anchor
            .as_ref()
            .filter(|_| !held)
            .and_then(|_| self.file_request(&seen));
        loop {
            if self.stopped() {
                return Ok(None);
            }
            self.check_slept(seen.screen.seq, review);
            self.dialog_note(&seen.screen, review);
            if self.host_steps_here(&seen, opts, review)? {
                return Ok(None);
            }
            // The host let the point go (its step owns nothing, its ownership
            // lapsed, `[harness] upgrade` switched off): the point is decided
            // again now, as at its deadline.
            if self.host_held && !opts.idle_host.as_ref().is_some_and(|h| h.owns_turn_end()) {
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
            // THE MEMORY CEILING while the loop waits: past it, a warning
            // said once an episode (and again when its verdict changes); at
            // an idle point decided without the Mac's trouble, a measure
            // acted on decides the point again now — the memory wall's
            // restart — and one no longer acted on hands back the act the
            // memory took: a wall's own, or a plain idle point's
            // continuation, its badge taken down (`memory_edge`).
            if self.memory_edge(&seen, opts, review)? {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            // THE ACCOUNTS MOVED UNDER A WAIT: the owner pinned one or renewed
            // a login, a login was found signed in, another session proved or
            // struck one — none of which moves this session's screen. The
            // host's stamp says so, and the point is decided again now, where
            // the host advises afresh (`account_edge`).
            if self.account_edge(opts) {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            // A look owed at the point still showing (a survey's `0` to try
            // again): the wait ends there, and the loop looks again.
            if self.look_at.is_some_and(|at| Instant::now() >= at) {
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
            let now = Instant::now();
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
                if let Some(anchor) = anchor.as_deref()
                    && self.caps.gone != Some(false)
                {
                    match self.wait(&["gone", anchor], step)? {
                        Wait::Latched => return Ok(None),
                        // The box's first row stood the whole step. A FILE
                        // box REPLACED under that row keeps it — a second
                        // Edit on the same path — and the wait never latches
                        // on it: one read says whether the request showing is
                        // still this one, and another is the next look's. What
                        // is compared is the REQUEST, never the box's review
                        // key: that key keeps a question's ticks, its typed
                        // text and a preview that follows the focus, and the
                        // letters above the box, so a person (or the manager)
                        // answering it, or a row the main thread prints over
                        // a subagent's box, would read as the box answered
                        // and asked again every step (measured: two asks for
                        // one ticked checkbox, on a held question's 2 s
                        // steps). Every other box is read again only when its
                        // row leaves.
                        Wait::TimedOut => {
                            if let Some(request) = request.as_deref() {
                                let screen = self.screen()?;
                                let now = self.turn_of(screen);
                                if self.file_request(&now).as_deref() != Some(request) {
                                    return Ok(None);
                                }
                            }
                        }
                        Wait::Unsupported => self.caps.gone = Some(false),
                    }
                } else if let Some(words) = self.agent_wake(&seen, &mut seen_key) {
                    match self.wait(&["agent", words], step)? {
                        Wait::Latched => {
                            self.agent_woke =
                                Some(seen_key.take().unwrap_or_else(|| self.review_key(&seen)));
                            return Ok(None);
                        }
                        Wait::TimedOut => {}
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
                if Instant::now() < until || self.stalled.is_some() {
                    continue;
                }
            }
            if Instant::now() >= *deadline {
                return Ok(Some(End::Timeout(seen)));
            }
            if self.look_at.is_some_and(|at| Instant::now() >= at) {
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
        // Only a wall's retry wait is a next try ([`Due::retry`]): the check
        // that the worker took the try just typed is told as no plan.
        let due = self.turn_end_due.as_ref().filter(|d| d.retry);
        let unix = |d: &Due| turn_end_loop::nearest_s(d.unix_ms, 0);
        let told = match (self.wait_told, due) {
            (Some(None), None) => return,
            (Some(Some(t)), Some(d)) if t.at == d.at && unix(d).abs_diff(t.unix) <= WAIT_SKEW_S => {
                return;
            }
            _ => due.map(|d| ToldWait {
                at: d.at,
                unix: unix(d),
            }),
        };
        let Some(host) = opts.idle_host.as_ref() else {
            return;
        };
        let plan = due.zip(told).map(|(d, t)| WaitPlan {
            at_unix: t.unix,
            why: d.why.clone(),
        });
        self.wait_told = Some(told);
        host.waiting(plan);
    }

    /// THE SESSION'S SLEEP WATCH: the two clocks read against the marks the
    /// last read left ([`Self::clock_marks`]). When the wall clock has run
    /// ahead of the monotonic one by more than a minute ([`slept_through`])
    /// the Mac slept that long, and the sleep is the session's AT ONCE: the
    /// policy's pauses and kept resets move by it ([`TurnEndState::slept`])
    /// before anything is stamped on the wake's side of it, and the wake's
    /// settle is placed from THIS read ([`Self::settle_wake`]) — whatever
    /// reaches the loop first after it, nothing typed into the agent goes
    /// before the settle falls ([`Self::settling`]). A due the sleep carried
    /// past is moved to the settle at the next place that can say so
    /// ([`Self::check_slept`]). Read after every request ([`Self::call`]: a
    /// sleep spans one, whatever answers it — a busy turn's reads among
    /// them), at each step of a wait, and where a decision takes its `now`.
    /// The marks move to now either way. A clock stepped BACK is no sleep.
    fn watch_sleep(&mut self) {
        let (mono_now, wall_now) = (Instant::now(), self.now_unix_ms());
        let Some((mono_then, wall_then)) = self.clock_marks.replace((mono_now, wall_now)) else {
            return;
        };
        let mono_ms =
            i64::try_from(mono_now.duration_since(mono_then).as_millis()).unwrap_or(i64::MAX);
        let wall_ms = wall_now.saturating_sub(wall_then);
        if !slept_through(wall_ms.div_euclid(1000), mono_ms.div_euclid(1000)) {
            return;
        }
        let by = u64::try_from(wall_ms.saturating_sub(mono_ms)).unwrap_or(0);
        self.turn_end.slept(Duration::from_millis(by));
        let woke_at = wall_now.div_euclid(1000);
        self.settle_wake(woke_at, mono_now);
        self.slept_at = Some(woke_at);
    }

    /// The turn-end policy's `now` for a decision: read after the watch
    /// ([`Self::watch_sleep`]), so a sleep is the policy's before the decision
    /// stamps anything on the wake's side of it.
    fn policy_now(&mut self) -> Instant {
        self.watch_sleep();
        Instant::now()
    }

    /// The watch read ([`Self::watch_sleep`]), and a wake it measured handed
    /// to [`Self::woke`]: at each step of a wait, before its deadline is read.
    fn check_slept(&mut self, seq: u64, review: &mut dyn Review) {
        self.watch_sleep();
        if let Some(wall_now) = self.slept_at.take() {
            self.woke(seq, wall_now, review);
        }
    }

    /// THE WAIT THE HOST WAS TOLD IS OVER ([`IdleHost::waiting`]`(None)`): the
    /// policy decides the point again ([`Self::turn_end_now`]) — the try is
    /// being typed — or the worker works ([`Self::await_turn_from`]). Told
    /// where the wait ends, not at the next `wait_for_next`: after a try the
    /// loop sits in the worker's whole busy turn, where nothing else tells,
    /// and a plan its early end left standing (an `Up` measure ended the rung,
    /// its time still ahead) was laid on the API wall the worker hit again
    /// seconds later. Nothing told, or told already: nothing.
    pub(super) fn end_told_wait(&mut self, opts: &SuperviseOpts) {
        if !matches!(self.wait_told, Some(Some(_))) {
            return;
        }
        self.wait_told = Some(None);
        if let Some(host) = opts.idle_host.as_ref() {
            host.waiting(None);
        }
    }

    /// A new measure from the host at a network wall: the cue to decide the
    /// point again at once ([`Self::wait_for_next`]). An `Up` continues within
    /// the step; a `Down` lost to `Unknown` returns the point to the unmeasured
    /// ladder; a new `Down` moves the wait out to the hold. So the loop decides
    /// at every change that can move the policy's answer, as
    /// `SupervisorNetworkWall` assumes. The host is asked only while the point
    /// is a network wall ([`Self::reach_seen`]) and a policy wait stands
    /// ([`Self::turn_end_due`]); elsewhere nothing is asked (an escalated point
    /// decided again would post its ask again) and the host's probe parks. A
    /// measure already seen (same `since`) is no edge; it is remembered before
    /// the point is decided.
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

    /// THE HOST'S ADVICE ABOUT THE SESSION'S ACCOUNT MAY HAVE CHANGED WHILE
    /// THE LOOP WAITS ([`IdleHost::account_stamp`]; the accounts design, §6
    /// "Every write that makes an account usable sooner touches a wake
    /// marker, so a tab waiting until Thursday decides again"): asked at the
    /// top of each step of a wait, and an edge — the point is decided again
    /// at once ([`Self::wait_for_next`]) — only where the decision that
    /// stands ASKED the host for its advice ([`Self::account_seen`]: a usage
    /// limit or a turn end where a move could be made) AND a wait of the
    /// policy's stands ([`Self::turn_end_due`]: an escalated point has
    /// nothing waited on, and deciding it again would post its ask again —
    /// [`Self::reach_edge`]'s rule). The stamp as this session last read it
    /// is remembered ([`Self::account_news`]), so a decision that reads
    /// nothing new is made once. No host, one whose stamp never moves (the
    /// trait's default), or `[harness] accounts = "off"` with `logins` off
    /// too: never an edge. (A renewal is news: the session waiting at its
    /// ended login decides again, and moves onto the renewed one.)
    ///
    /// A LATCHED SESSION WAITS ON NO ADVICE — the host refused its move for
    /// good, and it is not asked again while that wall stands
    /// ([`TurnEndState::account_refused`]) — and its wait hears the news all
    /// the same: the latch ends there ([`TurnEndState::account_news`]) and
    /// the point decided again asks afresh, so a remedy the owner applied
    /// (`accounts login`, `accounts use`) reaches it. A refusal with no news
    /// after it is not asked again; and news that follows a
    /// latched session's last within [`ACCOUNT_NEWS_EVERY`] waits that long
    /// (it is not lost: the stamp is read again at the next step). The
    /// stamp moves for what the host ITSELF wrote while it refused, too (a
    /// rotation committed and taken back): that costs one more ask, which a
    /// host that writes a repeated refusal once answers with no news — and
    /// one whose every refusal were news is asked for its relaunch once a
    /// minute, never as fast as it can refuse.
    ///
    /// A RENEWAL MADE WHERE THE POINT ASKED FOR IT is an edge with no wait
    /// standing too ([`Self::renewal_made`]): the escalation that asked for
    /// it, and its exception, are stale once it is made.
    pub(super) fn account_edge(&mut self, opts: &SuperviseOpts) -> bool {
        if opts.policy.accounts == crate::harness::accounts::Mode::Off && !opts.policy.logins {
            return false;
        }
        if self.renewal_made(opts) {
            return true;
        }
        let latched = self.turn_end.account_refused();
        if (self.account_seen.is_none() && !latched) || self.turn_end_due.is_none() {
            return false;
        }
        let Some(host) = opts.idle_host.as_ref() else {
            return false;
        };
        if latched
            && self
                .account_news_at
                .is_some_and(|at| at.elapsed() < ACCOUNT_NEWS_EVERY)
        {
            return false;
        }
        if !self.account_news(host.as_ref()) {
            return false;
        }
        if latched {
            self.account_news_at = Some(Instant::now());
        }
        true
    }

    /// THE RENEWAL THE POINT ASKED FOR IS MADE (accounts v2 S7): the point
    /// standing was decided on a lost login whose process held its row's own
    /// login ([`Self::login_seen`]), so its step was the renewal — and where
    /// nothing waits at that wall (no relaunch to follow the renewal with, a
    /// draft in the composer) that escalation stands with no wait of the
    /// policy's, which no other edge decides again. Once the host's stamp
    /// has moved past the one read with that answer, the host is asked
    /// again — once a move of the stamp, never at every step — and a login
    /// now superseded decides the point again (`true`): its badge and its
    /// exception say what still holds the tab, never the renewal again.
    /// News of anything else keeps the point as it stands.
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

    /// THE MEMORY CEILING, BETWEEN POINTS ([`IdleHost::memory`], judged by
    /// [`super::memory::judge`] under `[harness] memory_ceiling`), asked at
    /// the top of each step of a wait. Past the ceiling, in any phase, it is
    /// a WARNING said once an episode, and again when its verdict changes
    /// ([`Self::memory_warn`]), and nothing more. Only a measure ACTED ON ([`super::memory::MemoryOver::acts`]:
    /// the Mac in trouble, this agent the largest) at an IDLE point the last
    /// reading judged without one ([`Self::memory_seen`] `None`) — a wall's
    /// own idle point and a limit notice included, which may stand for days
    /// — is an edge: the point is decided again at
    /// once (`true`), where the turn-end reading carries the memory wall and
    /// the policy asks the host for its restart — so an agent the Mac's
    /// trouble came to while it sat idle is not left until its next turn
    /// ends, which may never come. No host, no measure, the ceiling off, a
    /// point decided on one already (its restart waits on its own
    /// back-off), a box or a question on the screen: nothing.
    ///
    /// The down edge: a point whose act the memory took (its reading took the
    /// memory wall in place of the screen's, [`Self::memory_over_wall`], or its
    /// escalation was the memory's, [`BoxAsk::memory`]) is handed back once the
    /// measure is no longer acted on. The memory badge is taken down (`CLEARED
    /// seq=<n> memory attention=<said>`, only if still ours) and the point
    /// decided again (`true`) by its own rule.
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

    /// THE MEMORY CEILING'S WARNING: nothing is ended, refused or held back
    /// — the loop decides every point as it would — but the measure is
    /// journaled `MEMORY seq=<n> …` and said to the host once an episode,
    /// and again whenever its verdict of what comes next changes within it
    /// ([`super::memory::Verdict`]: the row is keyed per tab, so the new one
    /// replaces the old — never `a warning only` over aterm's restart, nor a
    /// restart promised on a Mac that has recovered)
    /// ([`IdleHost::warn`]: a warning row on the window's Messages band),
    /// naming the footprint, the machine's state and what comes next
    /// ([`super::memory::MemoryOver::warning`]): nothing, with the tab's own
    /// resume line, on a Mac that is not in trouble or under an agent that
    /// is not the largest; aterm's restart at the next idle point where the
    /// measure is acted on and the host makes one; the person's where it
    /// makes none, or refused the last one asked for and has made none since
    /// ([`Self::memory_refused`]: the row then agrees with the badge's `the
    /// restart could not be made`). Said from the waits, and before the host
    /// is asked for the restart (`restart_agent`): the point's own reading
    /// may be the first to find the measure acted on.
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
        // Where the policy asks for a restart: a host that makes one, and
        // `[harness] relaunch` on as the loop's policy reads it
        // (`memory_restart` escalates with it off) — and not while the
        // host's last word on one is a refusal, whose badge names the
        // person's restart ([`Self::memory_refused`]).
        let restarts = host.can_restart() && opts.policy.relaunch && !self.memory_refused;
        self.memory_say(screen, over, restarts, opts, review);
    }

    /// [`Self::memory_warn`]'s row with `restarts` given: `true` once the
    /// host has made the restart (`restart_agent`'s `adopted`), whatever it
    /// says of the next one — a restart made over a refusal that stood names
    /// aterm's at once, on the measure it was made on, since the relaunched
    /// agent is a small one whose measures say nothing, and the person's
    /// `claude --resume` left standing would resume the conversation a
    /// second time.
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
            p @ (Program::Claude | Program::Codex | Program::Agy | Program::Muse) => p.product(),
            Program::Generic => "The agent",
        };
        let resume = host.resume_command();
        let text = over.warning(agent, restarts, resume.as_deref());
        review.note(&format!("MEMORY seq={} {text}", screen.seq));
        host.warn(&text);
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
    /// point read until the reset); the point the last such wait woke from,
    /// read again unchanged (the verdict and the reading disagree); a point
    /// the loop's reader does not vouch for
    /// ([`aterm_phase::Reading::phase_authoritative`]), e.g. read before the
    /// agent drew itself, whose verdict is already `idle` and never moves; and
    /// a point of an agent still starting (no mode or busy frame read yet,
    /// except Codex, agy and Muse Code, whose footers name none).
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
    /// `seen_key` is the review key of `seen`, built on first need and kept
    /// by the caller for the rest of its wait.
    fn agent_wake(&self, seen: &Turn, seen_key: &mut Option<String>) -> Option<&'static str> {
        if self.caps.await_agent != Some(true)
            || self.turn_end.act_in_flight()
            || self.turn_end.waits_on_named_reset()
        {
            return None;
        }
        if self.footer.is_none()
            && !self.busy_read
            && match self.reader(&seen.screen.rows).program() {
                Program::Claude | Program::Generic => true,
                Program::Codex | Program::Agy | Program::Muse => false,
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
    /// agent has not taken ([`TurnEndState::act_untaken`]) or open Codex
    /// save-then-wait switch, and only while the host [`IdleHost::wants`] a
    /// point; the host's act is journaled (`HOST seq=<n> background <word>`).
    /// Any other read restarts the settle.
    ///
    /// The guard is whether the act was taken, not whether its point was
    /// judged: a break reads busy, so an agent that orchestrates all day never
    /// reaches a judged point. A live turn read since the act is the agent
    /// taking it; only an untaken act holds the break back, so no host line is
    /// stacked on a continuation the agent has not read.
    fn host_steps_in_background(
        &mut self,
        screen: &Screen,
        since: &mut Option<Instant>,
        review: &mut dyn Review,
    ) {
        let reader = self.reader(&screen.rows);
        let unfit = if reader.phase(&screen.rows) != Phase::Busy {
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
            return;
        }
        let at = *since.get_or_insert_with(Instant::now);
        let Some(host) = host.filter(|h| h.wants()) else {
            self.withheld = None;
            return;
        };
        let held = if at.elapsed() < self.background_settle {
            Some(Guard::Settle)
        } else if self.limit.is_some() && !self.limit_spent(Some(host.as_ref()), notice) {
            Some(Guard::LimitEpisode)
        } else if self.stalled.is_some() {
            Some(Guard::Stall)
        } else if self.turn_end.act_untaken() {
            Some(Guard::ActUntaken)
        } else if self.turn_end.switch_open() {
            // Codex's save-then-wait switch owns the session until it
            // closes: no upgrade notice is typed into a wind-down or a hold,
            // as at an idle point ([`Self::host_steps_here`]).
            Some(Guard::SwitchOpen)
        } else if self.settling().is_some() {
            // Nor into the wake's settle: a busy read's request is what sees
            // the sleep there ([`Self::call`]).
            Some(Guard::WakeSettle)
        } else {
            None
        };
        if let Some(guard) = held {
            self.withhold(Some(&host), guard);
            return;
        }
        // Offered: the next guard, whatever it is, is said afresh.
        self.withheld = None;
        if let Some(line) = host.at_background() {
            review.note(&format!("HOST seq={} background {line}", screen.seq));
            // The notice is a turn the host typed: its answer is the
            // harness's, never the worker's work.
            self.turn_end.host_typed(Instant::now());
            *since = None;
        }
    }

    /// The host's step at `point` ([`IdleHost::at_idle`]), taken when the host
    /// asks for one ([`IdleHost::wants`]) and the point is idle as vouched for
    /// by the reader of the program the server names now (one `status`: a
    /// generic read of a bare composer is no evidence), read with the
    /// terminal's cursor ([`aterm_phase::read_at`]: Claude Code's `idle` only
    /// at the prompt box holding it), with no box, wall, open limit episode or
    /// act of the loop's own in flight. `true` when taken (the loop then reads
    /// again). The loop asks nothing of the server while the step runs.
    fn host_steps_here(
        &mut self,
        point: &Turn,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let Some(host) = opts.idle_host.as_ref().filter(|h| h.wants()) else {
            return Ok(false);
        };
        self.program = self.foreground_program()?;
        let reading = aterm_phase::read_at(
            self.program.as_deref(),
            &point.screen.rows,
            point.screen.cursor_index(),
            None,
        );
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
        // …and so is the notice a session MOVED TO ANOTHER ACCOUNT left
        // behind, repainted by the conversation it resumed, until a turn has
        // been asked under the new login
        // ([`TurnEndState::account_resume_owed`]): its reset is the OLD
        // account's, and neither the host's last step of the relaunch nor
        // the wall's resume is held for it.
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
        } else if reading.prompt.is_some() {
            Some(Guard::Prompt)
        } else if self.limit.is_some() && !spent {
            Some(Guard::LimitEpisode)
        } else if self.turn_end.act_in_flight() {
            Some(Guard::ActInFlight)
        } else if self.turn_end.switch_open() {
            // Codex's save-then-wait switch owns the session until it
            // closes: no upgrade notice is typed into a wind-down or a hold.
            Some(Guard::SwitchOpen)
        } else if self.settling().is_some() {
            // The Mac woke inside the settle: the step's turn would meet an
            // API the network may not reach yet. The wait offers the point
            // again when the settle falls ([`Self::wait_for_next`]).
            Some(Guard::WakeSettle)
        } else {
            None
        };
        let idle = guard.is_none();
        match guard {
            Some(guard) => self.withhold(Some(host), guard),
            // Offered: the next guard, whatever it is, is said afresh.
            None => self.withheld = None,
        }
        if idle {
            let step = host.at_idle();
            if let Some(step) = &step {
                if !step.repeat {
                    review.note(&format!("HOST seq={} {}", point.screen.seq, step.line));
                }
                // A turn the host typed is awaited as the policy's own acts
                // are, and its answer is no work of the worker's.
                if step.typed {
                    self.turn_end.host_typed(Instant::now());
                }
            }
            // Passed over for the step: decided again once the host owns
            // nothing — unless the step moved the session, and the point
            // with it.
            self.host_held = !step.is_some_and(|s| s.moved);
        }
        Ok(idle)
    }

    /// A usage limit left on screen past its reset is an idle point: `true`
    /// once the reset is [`crate::harness::upgrade_drive::LIMIT_GRACE_S`]
    /// behind now, by the episode's reset ([`Episode::reset_unix`]) or else the
    /// transcript's ([`IdleHost::limit_until`], asked only then). Claude Code
    /// leaves its notice up past the reset until something is typed, which
    /// would otherwise withhold every point for good. Past the grace neither
    /// the wall ([`Guard::Wall`]) nor the episode ([`Guard::LimitEpisode`])
    /// withholds a point (the episode stays open until the worker works), so
    /// the host steps as at any idle point and the policy continues the agent
    /// by the wall's rule. Before the grace, or with no reset read, everything
    /// withholds: a reset is never guessed.
    ///
    /// The reset is the standing notice's: `notice` is the usage-limit notice
    /// on screen now by its reset (`Some(None)`: naming none; `None`: no
    /// notice). The episode's reset counts only for the notice it was read
    /// from (same [`Episode::reset_text`]) or where no notice stands; an
    /// episode survives a retry, and a later live limit that names no reset
    /// must not be judged spent by the older reset. Such a notice is judged by
    /// the transcript's reset alone, which a rate limit's row does not name,
    /// so it withholds.
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

/// The journal's word for an escalation's post: the reply's own
/// ([`reply_word`]), except the two answers that mean queued. The server
/// records a post before it waits, so `ERR timeout id=<n>` (an `ask` under
/// `--wait=0`) and `ERR fabric <state> id=<n> …` (no bridge up) are
/// `queued id=<n> (…)`, not refusals.
fn post_word(r: &CtlReply) -> String {
    if !r.ok() {
        let err = r.err_text();
        let queued = if let Some(rest) = err.strip_prefix("ERR timeout ") {
            Some((rest, "not landed yet".to_string()))
        } else if let Some(rest) = err.strip_prefix("ERR fabric ") {
            let (state, rest) = rest.split_once(' ').unwrap_or((rest, ""));
            Some((rest, format!("fabric {state}")))
        } else {
            None
        };
        if let Some((rest, why)) = queued {
            let mut words = rest.split_whitespace();
            if let Some(id) = words.next().filter(|w| w.starts_with("id=")) {
                let more: Vec<&str> = words.collect();
                let why = if more.is_empty() {
                    why
                } else {
                    format!("{why}, {}", more.join(" "))
                };
                return clip(&one_line(&format!("queued {id} ({why})")));
            }
        }
    }
    reply_word(r)
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
/// left on its own — the fallback press's `1` after the prompt resolved, the
/// survey switch's `0` after the survey went (typed text, not the
/// placeholder the composer shows when empty).
fn stray_digit(reader: &dyn ScreenReader, screen: &Screen, digit: &str) -> bool {
    if reader.prompt(&screen.rows).is_some() {
        return false;
    }
    let composer = reader.composer(&screen.rows);
    typed_draft_in(composer.as_ref(), reader.placeholder(), screen)
        && composer.is_some_and(|(_, lines)| lines.len() == 1 && lines[0] == digit)
}

/// Text typed into the composer as `reader` reads it, not the placeholder it
/// shows when empty: a `0` pressed now could land in it. A draft can span rows
/// ([`ScreenReader::composer`]): text under the caret row is typed (the
/// placeholder is one row), and so is text on the caret row unless the cursor
/// sits at column 2 of that row, where every program parks it on an empty
/// composer — and there too on a program that draws no placeholder
/// ([`ScreenReader::placeholder`]: agy, Muse Code), whose words are all typed.
/// A cursor on no row read is judged by its column alone.
fn typed_draft(reader: &dyn ScreenReader, screen: &Screen) -> bool {
    typed_draft_in(
        reader.composer(&screen.rows).as_ref(),
        reader.placeholder(),
        screen,
    )
}

/// [`typed_draft`] of the composer as its reader already read it off
/// `screen` ([`ScreenReader::composer`]), `placeholder` its reader's
/// [`ScreenReader::placeholder`].
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

/// The line `watch` prints (and `supervise` says on stderr) when a rating card
/// appears: `EVENT survey seq=<n> dismiss with: aterm ctl @<sid> key
/// 'if=^●.How.is.Claude.doing' 0` — the guarded `0` that dismisses it, the one key
/// that is the manager's to press there (with no `@sid` named, none is in the
/// command either). The guard is the block's own ([`aterm_phase::SurveyBlock::guard`]):
/// `^●.` and the first words of the question row as the screen shows it, so it
/// follows the kind of card. Any kind but the session survey names itself after the
/// seq, `kind=plugin`. The guard is quoted: `^` is a glob operator in a zsh with
/// `extended_glob` set, where the bare word fails as `no matches found`. `EVENT`, so a
/// monitor that wakes on EVENT lines wakes on it: a turn typed while it is open whose
/// first character is 1, 2 or 3 is taken as a rating.
///
/// A card the harness will not press ([`aterm_phase::SurveyBlock::withheld`]) carries NO
/// command — `EVENT survey seq=<n> kind=unknown not pressed (<why>); read it with: aterm
/// ctl @<sid> text` — since a guard it does not have would be improvised by whoever runs
/// the line, and a `0` the loop withheld on purpose is not the manager's to press
/// unread.
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

/// What `phase` prints: [`render_phase`]'s lines, then `survey 0` when a rating card
/// is open ([`ScreenReader::survey_block`]) — the key that dismisses it — or, for a card
/// the loop would not press ([`aterm_phase::SurveyBlock::withheld`]: a shape nobody has
/// words for, or a question row no guard can fence), `survey withheld (<why>)`: a `0`
/// typed for it is a person's call, and the line says so instead of naming a key. A
/// rating is the human's; `0` is the one key a manager presses on a card that is
/// dismissable. Last, `context <n>%` when the program's context reading is up
/// ([`ScreenReader::context`]: Claude Code's indicator, Codex's footer): that much is
/// left before the worker auto-compacts. Without either, the lines are
/// [`render_phase`]'s alone. Every line is `reader`'s, the worker's own program's.
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

/// The lines `phase` / `await-turn` print, read by `reader` (the worker's own
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
/// line that says what Esc does on it: Claude Code's [`parse_prompt`] (a note
/// per row; `cancel esc`, Esc leaving every box), any other program's
/// [`ScreenReader::prompt`] (Codex's box: its kind, command or folder, its
/// question, its numbered options) and its footer's Esc ([`esc_line`]).
fn phase_prompt(reader: &dyn ScreenReader, rows: &[String]) -> Option<(Prompt, &'static str)> {
    match reader.program() {
        Program::Claude => parse_prompt(rows).map(|p| (p, ESC_CANCELS)),
        Program::Codex | Program::Agy | Program::Muse | Program::Generic => {
            reader.prompt(rows).map(|p| {
                let esc = esc_line(&p);
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

/// `phase`'s line for a box whose Esc leaves it (a refusal, a step back, or
/// — the trust boxes' convention — the program's exit).
const ESC_CANCELS: &str = "cancel esc";

/// What Esc does on a box another program's reader read, as `phase` prints
/// it: `interrupt esc` where the box's footer says Esc interrupts — Codex's
/// question (`esc to interrupt`): Esc there ends the whole turn (`■
/// Conversation interrupted`, measured on 0.156.1 and 0.163.0) and declines
/// nothing; else [`ESC_CANCELS`].
fn esc_line(p: &PromptV2) -> &'static str {
    match p.cancel.as_ref().map(|c| c.effect) {
        Some(CancelEffect::Interrupt) => "interrupt esc",
        Some(CancelEffect::Reject | CancelEffect::Exit | CancelEffect::Back) | None => ESC_CANCELS,
    }
}

/// Which busy rule fired on `rows`, read by `reader`, as `<where>: <rule>`:
/// Claude Code's [`busy_signal`] (any program but Codex: its rules are the
/// only ones a screen with no reader's grammar is scanned by); Codex's
/// status line over a background terminal it left running
/// ([`ScreenReader::background_wait`]), its status row (the reader's
/// [`ScreenReader::busy_guard`], `esc to interrupt)`), or the last message
/// with no end row under it — the answer streaming ([`aterm_phase::codex`]).
fn busy_rule(reader: &dyn ScreenReader, rows: &[String]) -> Option<String> {
    match reader.program() {
        Program::Claude | Program::Generic => busy_signal(rows).map(|b| b.to_string()),
        // Their own reader's: its status row, else its reading of the screen.
        Program::Agy | Program::Muse => {
            let status_row = reader.busy_guard().is_some_and(|guard| {
                aterm_observe::row_matcher(&guard).is_ok_and(|m| rows.iter().any(|r| m.matches(r)))
            });
            if status_row {
                return Some("status row: esc to interrupt".to_string());
            }
            (reader.phase(rows) == Phase::Busy).then(|| "its reader reads it busy".to_string())
        }
        Program::Codex => {
            if let Some(what) = reader.background_wait(rows) {
                return Some(format!("status line: {what}"));
            }
            let status_row = reader.busy_guard().is_some_and(|guard| {
                aterm_observe::row_matcher(&guard).is_ok_and(|m| rows.iter().any(|r| m.matches(r)))
            });
            if status_row {
                return Some("status row: esc to interrupt".to_string());
            }
            (reader.phase(rows) == Phase::Busy)
                .then(|| "transcript: the last message has no end row under it".to_string())
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
    event_line_as(turn.phase.name(), turn, python_allow, None)
}

/// `EVENT <word> seq=<n> [complete=<0|1> rows=<n>] <summary>`: the line of
/// a review point under the phase word given (`idle-no-report` for an idle
/// point `--mail` waited on in vain), with the brief `--report` read, if any.
fn event_line_as(
    word: &str,
    turn: &Turn,
    python_allow: &[String],
    report: Option<ReportBrief>,
) -> String {
    match report {
        Some(brief) => format!(
            "EVENT {word} seq={} complete={} rows={} {}",
            turn.screen.seq,
            u8::from(brief.complete),
            brief.rows,
            event_summary(turn, python_allow)
        ),
        None => format!(
            "EVENT {word} seq={} {}",
            turn.screen.seq,
            event_summary(turn, python_allow)
        ),
    }
}

/// What `watch --report` adds to an EVENT line: whether `report` found
/// nothing lost since the manager's turn, and how many rows it holds — and,
/// for the journal, the ledger turn it counted from (`None` when it started
/// at no turn, or failed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportBrief {
    pub complete: bool,
    pub rows: usize,
    pub turn: Option<u64>,
}

/// The review points `watch --report` counts a report for: the turn ended on
/// words — idle, a question, a limit notice — not on a box or still busy.
fn reported(phase: &Phase) -> bool {
    matches!(phase, Phase::Idle | Phase::Question | Phase::Limited { .. })
}

/// [`event_line`] with `complete=<0|1> rows=<n>` between the seq and the
/// summary (`watch --report`); the summary stays the free-text tail.
pub fn reported_event_line(turn: &Turn, python_allow: &[String], report: ReportBrief) -> String {
    event_line_as(turn.phase.name(), turn, python_allow, Some(report))
}

/// What a point's line says of it, read by the reader the SCREEN names
/// ([`aterm_phase::identify`] — a caller of [`event_line`] has no session to
/// ask): a box's kind, verdict and command; else what [`said_summary`] says.
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
/// message and reset; else the worker's last row (the last row said as drawn
/// — Claude Code's `⏺` row, a shell's prompt; Codex's, agy's and Muse Code's
/// last words' last line);
/// a prompt whose box `reader` does not read, that it is a box of no kind.
fn said_summary(reader: &dyn ScreenReader, turn: &Turn) -> String {
    let rows = &turn.screen.rows;
    match &turn.phase {
        Phase::Prompt => "kind=other classify=- command=-".to_string(),
        Phase::Limited { message, reset } => format!(
            "message={} reset={}",
            clip(message),
            reset.as_deref().map_or_else(|| "-".to_string(), clip)
        ),
        Phase::Busy | Phase::Idle | Phase::Question => match reader.program() {
            Program::Codex | Program::Agy | Program::Muse => or_dash(&clip(
                reader
                    .said_tail(rows)
                    .as_deref()
                    .and_then(|t| t.lines().last())
                    .unwrap_or("")
                    .trim(),
            )),
            Program::Claude | Program::Generic => {
                or_dash(&clip(last_said_row(rows).unwrap_or("").trim()))
            }
        },
    }
}

/// What makes two review points the same one — never the seq, which any
/// footer tick moves — read by `reader`, the loop's ONE grammar
/// ([`Session::review_key`] passes [`Session::reader`]: the reader
/// [`box_anchor`] and [`Session::turn_of`] read the same rows by, never the
/// screen's guess alone, which may name another program than the session's).
///
/// A BOX `reader` reads is the request it makes ([`box_key`]), never how it
/// is drawn. Any other point — and a prompt whose box `reader` does not read
/// — is the phase, what it says ([`said_summary`]), the status row, and the
/// words of the last [`KEY_ROWS`] non-blank rows of the transcript, up to the
/// last thing said or, for that prompt, up to its box ([`prompt_box_span`]),
/// whose rows count verbatim. Anything the worker, a tool or the manager adds
/// lands in those rows (a retry after its `❯` row, a new reply), while a
/// footer or a banner does not; letters only, so a timer that ticks or a
/// glyph that blinks does not make a row new.
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

/// The review key of the BOX `p`, read on `rows`: WHAT IT ASKS, never how it
/// is laid out. A key that counts the drawing (rows verbatim, focus marker,
/// countdown, a command split that moves with a wrap) reads a redraw of the
/// SAME box as the box answered and a new one raised: `CLEARED` then
/// `ESCALATED`, the manager asked again. Four lines:
///
/// 1. `prompt <kind> <whole|cut>`, and ` form=<single|multi|preview|review|
///    unknown>` for a question dialog: the kind the reader named, and
///    whether it read the box WHOLE — its title on the screen, and not a
///    question named by its footer or options alone.
/// 2. Its options' numbers, in order (`-` for one drawn with none).
/// 3. ITS BODY'S DIGEST: every row [`approval_loop::box_rows`] keeps — the
///    title (origin and `(unsandboxed)` with it, so a Write and an Edit on
///    one path differ), each command row, each diff and preview row with its
///    line numbers and `+`/`-`, the description, the notes, the reason
///    block, the question, each option's label, the footer — reduced to its
///    CONTENT characters ([`is_layout`] drops whitespace and box drawing) and
///    run together with no separator. A row broken at another point — a soft
///    wrap at a space, a hard one mid-token — reads the same, and so does a
///    rule or a `╌` divider whose length follows the width. What the box
///    asks is in it whole: another command, another diff, another note,
///    another option's label is another digest — two Edit boxes on one path
///    with different diffs are two points.
/// 4. WHAT CAME RIGHT BEFORE IT: for a box read whole, the last
///    [`ABOVE_LETTERS`] letters of the transcript above the pending call's
///    echo ([`before_the_echo`]: the vendor's own rows right over the box,
///    which repeat its request), run together — what the worker said or ran
///    before it asked. Letters only and
///    no row breaks, so the transcript re-wrapped at another width, the tool
///    row's blinking `⏺`, and older rows scrolling into view at the top of
///    the pane as the box shrinks (the measured redraw, `cap-taken-box-2`)
///    change nothing; output the worker appended since does — the same
///    `git push` asked again after it failed (the reviewers' live case),
///    where the push and the model's turn ran inside the loop's settle and
///    no read saw the busy spell that would make the box a new point.
///
/// Not in it: the auto-deny countdown, the focus marker `❯` and blank rows
/// ([`approval_loop::box_rows`]), and the `›` Codex draws on its focused
/// option — so a ticking, arrowed-through or blank-wiped box is the same
/// point. Checkbox marks, typed free text and a preview pane that follows the
/// focus are drawn content, and stay in — which is why nothing that looks at a
/// box WHILE it waits compares this key: the wait's one read under a file
/// box's row compares what the box asks alone ([`file_request_key`]), and a
/// question or a command is read again only when its row leaves, so a person
/// ticking, typing or arrowing through it is never asked about again.
///
/// Where the reader could not read the box whole — its head off the screen,
/// or a question named by its footer or options alone, whose question stands
/// above what the reader took for the box — line 4 is the words of the last
/// [`KEY_ROWS`] non-blank rows above its span, one per line, as for any other
/// point, and such a box redrawn with another window of it visible is a new
/// point.
///
/// The residuals. Two boxes that differ only in whitespace (`rm -rf /tmp/x /`
/// and `rm -rf /tmp/x/`), drawn back to back with no busy read between them
/// and nothing said between, are one point — no second ask, the badge naming
/// the first: no form that holds across a wrap can keep a token boundary (a
/// soft wrap consumes the space, and a hard one inserts none). A pane so
/// short that fewer than [`ABOVE_LETTERS`] letters stand above a whole box's
/// echo reads a row scrolling in at its top as a new point. And a box looked
/// at again while it stands (after an outage, say) whose transcript above its
/// echo moved — the main thread printing over a subagent's box — is a new
/// point.
/// The key only ever passes over a point already handed over; it never
/// causes a press.
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

/// The transcript above a box (`above`: every row over its title) less the
/// pending call's ECHO — the vendor's own row group right over the box,
/// which repeats what the box asks: `Running 2 shell commands…` over `⎿  $
/// <command>` (2.1.283, `cap-taken-box-1`), `Removing directories …` over
/// the same (2.1.280's rm box), `⏺ Creating file x` over `⎿  $ touch x`,
/// `⏺ Update(notes.txt)` (2.1.280's Edit box) — and Codex's `• Running
/// <command>` (0.156.1's exec box) — and the rule and blank rows between it
/// and the box ([`box_key`]'s line 4). The group is the run of rows with
/// content right over those, and it is blank-separated from what the worker
/// said before it in every layout measured; a group with NO gap row above it
/// — a pane whose transcript has no blank row at all, as the live recorder
/// keeps it (`BOX_RM_AUTO_DENY`), or an echo whose top is cut off at the
/// pane's top — is not known to be an echo and stays.
///
/// Why: the window right over the title is the echo, letter for letter. In
/// 2.1.283's layout a command of 44 letters or more leaves nothing else in
/// [`ABOVE_LETTERS`] (64, less the 20 of `Running N shell commands`), so the
/// same command asked again after it failed — the worker's output and its
/// next words appended above the new echo — would read as the box asked
/// before, and never be asked (measured on the 2.1.283 capture with a
/// 55-letter `cargo test`). For a
/// subagent's box the group is the main thread's spinner row (`✻ Waiting for
/// 1 background agent to finish`), which nothing in the box repeats either.
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

/// A character that is LAYOUT, not content ([`box_key`]): whitespace — a
/// wrap point, an indent, a gutter's padding, a no-break space — and the
/// box-drawing block (U+2500–U+257F): the `│` gutter bars, the `─` rules and
/// the `╌` diff dividers, whose length follows the pane's width.
fn is_layout(c: char) -> bool {
    c.is_whitespace() || ('\u{2500}'..='\u{257F}').contains(&c)
}

/// What a FILE box asks — an Edit (a notebook's and an IDE diff's with it),
/// a Write, an Overwrite or a Read box `reader` reads whole — for the one
/// read after a step its first row stood ([`Session::wait_for_next`]): its
/// kind, then [`box_key`]'s digest of its rows ABOVE its first option — the
/// title, the path, every diff or preview row, the question — and nothing
/// else. `None` for any other point.
///
/// The kinds are the ones whose first body row — the row the wait is
/// anchored on ([`box_anchor`]), the path — two requests share: two Edit
/// boxes on one file with different diffs, drawn back to back, are one row
/// to `await gone`, and only a read sets them apart. A command, a url, a
/// question opens with what it asks, so another request under the same row
/// is not the case this read exists for: those are read again when their row
/// leaves. (A command long enough that two share a whole first row is not
/// seen until that row leaves.)
///
/// Left out, each because it moves while the SAME request stands: the
/// options — the focus marker, a `Tab to amend` text typed into one, a
/// grant's label — and the footer; the auto-deny countdown
/// ([`approval_loop::box_rows`]); and the letters above the box
/// ([`box_key`]'s line 4), which the main thread prints into over a
/// subagent's box. So a person, or the manager, part-way through answering
/// the box ends no wait, and neither does a redraw.
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

/// An informational line on a diagnostic stream (`supervise`'s and
/// `await-turn`'s stderr): flushed, and a failed write is not the loop's end.
fn log_line(log: &mut dyn Write, line: &str) {
    let _ = emit(log, line);
}

/// Write one line and flush it: the harness reading `watch` wakes per line.
fn emit(out: &mut dyn Write, line: &str) -> Result<(), String> {
    writeln!(out, "{line}")
        .and_then(|()| out.flush())
        .map_err(|e| format!("cannot write a line to stdout: {e}"))
}

fn render_prompt(p: &Prompt, esc: &str, python_allow: &[String]) -> String {
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
    // select's cancel.
    out.push_str(esc);
    out.push('\n');
    out
}

/// [`render_result`] as `supervise --mail` prints it: after the phase lines,
/// `report <id> rows=<n>` for an idle point a report was folded into, `report
/// -` for one no report came for; without a fold, [`render_result`] unchanged.
pub fn render_result_mail(
    turn: &Turn,
    reader: &dyn ScreenReader,
    python_allow: &[String],
    fold: Option<Fold>,
) -> String {
    let mail = match fold {
        Some(Fold::Report { id, rows }) => format!("report {id} rows={rows}\n"),
        Some(Fold::NoReport) => "report -\n".to_string(),
        None => String::new(),
    };
    let mut out = render_phase(turn, reader, python_allow);
    out.push_str(&mail);
    out.push_str(&render_result_rows(turn));
    out
}

/// The compact result `supervise` prints: the phase lines (read by `reader`,
/// [`render_phase`]), then the prompt box verbatim, or the last 28 non-blank
/// rows.
pub fn render_result(turn: &Turn, reader: &dyn ScreenReader, python_allow: &[String]) -> String {
    let mut out = render_phase(turn, reader, python_allow);
    out.push_str(&render_result_rows(turn));
    out
}

/// The `--` line and the rows under it ([`render_result`]).
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

/// Append `<utc stamp> <line>` to the notes file, if one was given.
fn append_note(path: Option<&Path>, line: &str) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    use std::io::Write as _;
    let secs = aterm_time::unix_secs().unwrap_or(0);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open notes file {}: {e}", path.display()))?;
    writeln!(f, "{} {line}", utc_stamp(secs))
        .map_err(|e| format!("cannot append to notes file {}: {e}", path.display()))
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
#[path = "claim.rs"]
mod claim;
pub use claim::{CLAIM_HELD, CLAIM_RENEW};
#[path = "escalate.rs"]
mod escalate;
#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "press.rs"]
mod press;
#[path = "reset_loop.rs"]
mod reset_loop;
#[path = "stall.rs"]
mod stall;
#[path = "turn_end_loop.rs"]
mod turn_end_loop;
#[path = "wall_confirm.rs"]
mod wall_confirm;
#[path = "wall_memory.rs"]
mod wall_memory;
/// The composer's cursor-row guard, for `aterm drive task`'s nudge too, and
/// the live upgrade's pressed notice (`harness::upgrade_drive`).
pub(crate) use turn_end_loop::composer_guard;
pub use turn_end_loop::standing_rules;

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
