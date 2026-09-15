// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor loop over the control verbs: wait for the worker's turn to
//! end ([`Session::await_turn`]), then either approve a read-only Bash prompt
//! itself or hand the screen to the manager — once, and exit
//! ([`Session::supervise`]), or as one line per decision while it keeps
//! watching ([`Session::watch`]). Every request goes through one [`Ctl`] seam
//! — the `aterm-ctl` launcher the other drive subcommands use in production, a
//! scripted mock in the tests — and the server features newer builds add
//! (`text … tail=`, `await gone`, `key if=`) are probed ONCE and remembered, so
//! the loop runs against an older host too. A request the server did not serve
//! — the instance hosting the session went away under it, as it does when an
//! aterm self-update hands every session to the new instance under the same
//! `@sid`, or the server turned the connection away — does not end the loop:
//! the outage is ridden out ([`Session::ride_out`]) and the loop looks again
//! from a fresh read.
//!
//! With `--mail` ([`SuperviseOpts::mail`]) a second [`Ctl`] — the mail lane,
//! [`super::mail::Lane`] — parks `await inbox` on the MANAGER's own session
//! from a thread of its own, prints `MAIL …` per delivery through the loop's
//! one sink, and hands the watched worker's `report`s to the loop, which
//! folds each into the idle point of the same turn
//! (`Session::hold_for_report`): one `EVENT turn` line per worker turn. The
//! worker's socket sees not one request more than without the flag, and
//! without it every line is byte-identical.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::classify::{DEFAULT_PYTHON_ALLOW, Verdict, classify_command_with};
use super::journal::Journal;
use super::mail::{self, Delivery, MailOpts};
use super::phase::{
    Phase, busy_signal, composer_draft, composer_text, context_left, has_composer_frame,
    is_placeholder, last_said_index, last_said_row, status_row, survey_open, worker_phase,
};
use super::prompt::{Prompt, PromptKind, parse_prompt, prompt_box_span};
use super::report::ReportOpts;
use super::screen::{Screen, parse_text_json};

/// One `aterm-ctl` exchange: the exit code, stdout and stderr.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CtlReply {
    /// The client's exit code: 0 ok, 124 a timeout, 1 an error (`ERR …` on stderr).
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
    /// A bare `ERR` with nothing after it — what a build that predates a verb's
    /// leading option answers when the option is read as a key name (measured:
    /// aterm 0.81.0 answers `key if=Do.you.want.to.proceed 1` and `key
    /// nosuchkey` alike with `aterm-ctl: ERR`, exit 1, stdout empty).
    pub fn bare_err(&self) -> bool {
        !self.ok() && self.err_text() == "ERR"
    }
    /// The server does not know this form: a usage line or a bare `ERR`.
    pub fn unknown_form(&self) -> bool {
        self.usage_error() || self.bare_err()
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
    /// was written.
    pub fn skipped(&self) -> bool {
        self.ok() && self.stdout.trim_start().starts_with("OK skipped")
    }
    /// The request was not served: the client could not reach the instance
    /// hosting the session, or lost it mid-exchange — the connection closed
    /// before a reply (`server closed the connection without responding`,
    /// measured when aterm 0.82.0 handed its sessions to 0.83.0 under a running
    /// `watch`), a reply cut short, a socket nothing listens on or that is gone,
    /// a reset, a broken pipe, a socket timeout — or the server turned the
    /// connection away before it read the request (`TURNED_AWAY`: its
    /// admission queue full, or the token sent another instance's). Any other
    /// `ERR …` line is the server's answer and never this: `ERR no such
    /// session` and `ERR exited` say the session is gone (though while an
    /// outage is being ridden out the loop takes `no such session` for one
    /// more request not served — `Session::unserved`).
    pub fn lost(&self) -> bool {
        if self.ok() {
            return false;
        }
        let lines: Vec<&str> = self
            .stderr
            .lines()
            .map(|l| {
                let l = l.trim();
                l.strip_prefix("aterm-ctl:").map_or(l, str::trim)
            })
            .filter(|l| !l.is_empty())
            .collect();
        match lines.iter().find(|l| l.starts_with("ERR")) {
            Some(err) => TURNED_AWAY.iter().any(|m| {
                err.strip_prefix(m)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', ';']))
            }),
            None => lines.iter().any(|l| {
                LOST.iter().any(|m| l.contains(m))
                    || (l.starts_with("connect ") && LOST_SOCKET.iter().any(|m| l.contains(m)))
            }),
        }
    }
}

/// What the client prints when a request never got the server's answer
/// ([`CtlReply::lost`]): `aterm-ctl`'s own words for a connection that closed
/// before the reply or during it, and the OS's for a refused, reset or broken
/// connection and a socket timeout (the Unix and the Windows spellings).
const LOST: &[&str] = &[
    "server closed the connection without responding",
    "server hung up before the complete response",
    "Connection refused",
    "actively refused",
    "Connection reset",
    "forcibly closed",
    "Broken pipe",
    "Resource temporarily unavailable",
    "timed out",
];
/// A socket path that is gone, as the client's `connect <path>: …` line says
/// it: the instance that bound it has exited.
const LOST_SOCKET: &[&str] = &["No such file or directory", "cannot find the file"];
/// The server's `ERR` lines that turn a connection away BEFORE its request is
/// read, so nothing was served ([`CtlReply::lost`]), each a whole leading
/// phrase: `ERR control server busy; retry` (the listener's admission queue is
/// full — an explicit retry signal) and `ERR auth` (the token sent is not the
/// one this instance holds: the `latest` alias moved between the client's
/// connect and its token read, or the instance restarted — the client re-reads
/// the token on its next run).
const TURNED_AWAY: &[&str] = &["ERR control server busy", "ERR auth"];

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
}

/// A turn's end: the phase it ended in and the screen it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub phase: Phase,
    pub screen: Screen,
    /// The budget ran out with the phase still busy.
    pub timed_out: bool,
}

/// `supervise`'s knobs (and `watch`'s: the same loop).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SuperviseOpts {
    /// Approve (option 1) a Bash prompt whose command classifies read-only.
    pub auto_reads: bool,
    /// The wall-clock budget.
    pub max: Duration,
    /// `python3 <script>` globs that count as reads (empty = the default list).
    pub python_allow: Vec<String>,
    /// A notes file to append one line per action to.
    pub notes: Option<PathBuf>,
    /// `watch --report`: an idle, question or limited point's EVENT line
    /// carries `complete=<0|1> rows=<n>` of `report` before its summary.
    pub report: bool,
    /// `--dismiss-surveys`: when Claude Code's session survey appears, press
    /// `0` on it with the guarded press (`key if=^●.How.is.Claude.doing 0`)
    /// and, once it has left, say `DISMISSED survey seq=<n>`, instead of
    /// saying `EVENT survey …`. `0` only, ever: a rating is the human's to give.
    pub dismiss_surveys: bool,
    /// `--context-warn`: the percentage of the worker's context left
    /// ([`context_left`]) at or below which the loop says `EVENT context
    /// seq=<n> <v>% until auto-compact`, once a descent, and after which the
    /// indicator gone from a framed screen with no box up (or reading 30
    /// points or more higher) is said as `EVENT compacted seq=<n>`. `0` says
    /// neither — the default here; the CLI's is 10. The watch lasts one loop,
    /// armed at its start: a compaction between two `supervise` runs is not
    /// seen.
    pub context_warn: u8,
    /// `--journal`: a file to append one JSON object to for every line the
    /// loop prints — and, for `supervise`, for every line `watch` would have
    /// printed for what it decides silently ([`super::journal`]).
    pub journal: Option<PathBuf>,
    /// `--mail`: park the mail lane ([`super::mail::Lane`]) on the manager's
    /// inbox beside the loop. Every delivery prints `MAIL id=<n> off=<o>
    /// from=<sid> kind=<k> len=<n> [re=<o>]` as it lands; an idle point is
    /// held (`Session::hold_for_report`) for the worker's `report` of the
    /// same turn — `idle_grace` at most, the screen read once a step under
    /// it — and printed as ONE line, `EVENT turn seq=<n> report=<id>
    /// rows=<n> <summary>` (the report's row id and its body's row count)
    /// when one that is the turn's came ([`MailIn::is_this_turns`]:
    /// `report_window` bounds only a report from before the turn was seen to
    /// begin), else `EVENT idle-no-report seq=<n> <summary>`. `None` — no
    /// flag — leaves every line as it was.
    pub mail: Option<MailOpts>,
}

impl SuperviseOpts {
    fn allow(&self) -> Vec<String> {
        if self.python_allow.is_empty() {
            DEFAULT_PYTHON_ALLOW.iter().map(|s| s.to_string()).collect()
        } else {
            self.python_allow.clone()
        }
    }
}

/// How many rows a screen read asks for when the server can tail.
const TAIL_ROWS: &str = "40";
/// The longest single wait, so the budget is re-checked between waits.
const WAIT_STEP: Duration = Duration::from_secs(20);
/// The idle window that means "the screen settled".
const IDLE_MS: &str = "2000";
/// The busy footer whose LEAVING ends a turn (one token: the wire never quotes).
const BUSY_FOOTER: &str = "esc.to.interrupt";
/// The row `key if=` requires before pressing an option.
const PROCEED: &str = "Do.you.want.to.proceed";
/// The row `key if=` requires before pressing `0` on the session survey: its
/// question row, `●` in column 0. The server tests the pattern against EVERY
/// row on the screen, and a copy of the survey — a tool's output under the
/// `⎿` gutter, the worker quoting it, this very pattern in an EVENT line or
/// in `--help` — does not start its row with `●`, so a copy on the screen
/// when the survey itself has gone lets nothing through to the composer. The
/// one copy that does match is a worker message that opens with the question
/// itself on a platform that draws the message glyph as `●`, not `⏺`.
const SURVEY_ROW: &str = "^●.How.is.Claude.doing";
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
/// retried this many times, this far apart, before it is an error.
const BUSY_SINK_RETRIES: u32 = 3;
const BUSY_SINK_BACKOFF: Duration = Duration::from_millis(250);
/// The same read-only prompt is approved this many times; the next time it
/// comes back it is the manager's (a command that keeps failing and being
/// retried is not a read the supervisor should keep waving through).
const MAX_APPROVALS_OF_ONE_COMMAND: usize = 2;
/// How many rows the compact result prints when there is no box.
const RESULT_ROWS: usize = 28;
/// How many characters of a command, a said row or a notice one `watch` line
/// carries.
const LINE_CHARS: usize = 160;
/// How many non-blank rows of the transcript, up to the last thing said (or
/// up to a prompt box), make a review point's words ([`review_key`]).
const KEY_ROWS: usize = 8;

/// The exit code `supervise`, `watch` and `await-turn` end with on a spent
/// budget.
pub const EXIT_TIMEOUT: u8 = 124;

/// How long an outage is ridden out before it ends the loop, unless
/// `--reconnect-s` says otherwise.
const DEFAULT_RECONNECT: Duration = Duration::from_secs(180);
/// The pause before the first try to reach the session again; each pause
/// doubles, up to [`RECONNECT_PAUSE_MAX`], across all of an outage's
/// ride-outs.
const RECONNECT_PAUSE: Duration = Duration::from_millis(500);
const RECONNECT_PAUSE_MAX: Duration = Duration::from_secs(8);

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
}

/// An outage: from the first request the server did not serve until the loop
/// gets past it ([`Session::call`], [`Session::drive`]). However many
/// ride-outs it takes, it is said once and bounded by one reconnect window.
#[derive(Debug)]
struct Outage {
    /// When that first request came back unserved: the window runs from here.
    since: Instant,
    /// Its kind, the verb and the verb's first word (`await seq`, `key
    /// if=Do.you.want.to.proceed`, `text --json`): a request of the same kind
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
    /// The lane ended (its `MAIL lane off` line said why): no report can
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
    /// Whether a delivery is the turn's that ends at the point being held
    /// (`now`): after the turn was seen to begin (`busy_at`) it is, whatever
    /// its age; between a handed-over point and that read it is the ended
    /// turn's late report (the Stop hook fires a moment after the screen
    /// settles) and is not; before the turn was seen to begin — the loop's
    /// first turn, or one too short to be read busy — it is when it came
    /// within the window, the only thing then known about it.
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

/// Where every line of a loop goes — `watch`'s stdout, `supervise`'s stderr —
/// each recorded in the journal first. ONE per loop, shared with the mail
/// lane's thread under a lock, so a `MAIL` line and an `EVENT` line never
/// interleave mid-line and the journal's order is the printed order. The
/// lock is taken in the open at every site (`sink.lock()`, poisoned or not:
/// a line is worth printing after a panic elsewhere), held for one line, and
/// nothing else is locked under it.
struct Sink<'w> {
    out: &'w mut (dyn Write + Send + 'w),
    journal: Journal,
    /// Where the journal's one warning goes (`watch`: stderr); `None` sends
    /// it to `out` (`supervise`, whose lines are stderr already).
    warn: Option<&'w mut (dyn Write + Send + 'w)>,
}

impl Sink<'_> {
    /// Record `line` in the journal (its warning to `warn`, else to `out`).
    fn record(&mut self, line: &str, turn: Option<u64>) {
        match self.warn.as_deref_mut() {
            Some(w) => self.journal.record(line, turn, w),
            None => self.journal.record(line, turn, self.out),
        }
    }

    /// Record `line`, then print and flush it.
    fn line(&mut self, line: &str, turn: Option<u64>) -> Result<(), String> {
        self.record(line, turn);
        emit(self.out, line)
    }
}

/// What the shared loop ([`Session::drive`]) carries from one look to the
/// next.
struct Looking {
    /// The read-only commands approved since the last review point.
    approved: Vec<String>,
    /// The first wait of the next turn: the busy footer leaving, except right
    /// after a press, a review point or a reconnect, when it is no signal.
    gone_first: bool,
    /// The review point last handed over, until the worker moves.
    handed: Option<String>,
    /// A read since the last turn's end saw the worker busy. Kept here, not
    /// in one wait's locals, so a busy spell read before a lost connection
    /// still counts after it.
    moved: bool,
    /// Where the session survey stood at the last look ([`Session::survey`]).
    survey: Survey,
    /// A screen the last look read past its point (the hold's safety net,
    /// [`Session::hold_for_report`]): the next look's turn, if it still shows
    /// a point.
    carried: Option<Screen>,
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
    /// `--dismiss-surveys` tried its guarded `0`: `pressed` is the server's
    /// stamp on a `0` it wrote, `None` a press it skipped. The next look
    /// without a box or a draft says what came of it.
    Tried { pressed: Option<u64> },
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

/// What [`Session::auto_read`] made of a turn.
enum Step {
    /// A read was approved, or the box had left: look again. `settle` makes
    /// the first wait the settle wait, not `await gone` (the busy footer is not
    /// up yet right after a press).
    Again { settle: bool },
    /// A review point: the manager's.
    Review(Turn),
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
}

/// The EVENT line of a review point, as `--report` and `--mail` shape it:
/// [`reported_event_line`]'s with a brief, [`event_line`]'s without; an idle
/// point `--mail` folded a report into is `EVENT turn seq=<n> report=<id>
/// rows=<n> <summary>` (the brief, had one been read, would count the same
/// words twice: none is), and one no report came for is `EVENT idle-no-report
/// …` with the brief where there is one.
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
    /// The `1` was written while the box was up.
    Pressed { seq: u64 },
    /// Nothing was written: the box was no longer on the screen at the check
    /// (or, on the fallback path, the digit landed in the composer and was
    /// backspaced — the prompt had resolved on its own).
    Skipped { seq: u64 },
}

/// A guarded press, and whether the connection held until it was seen
/// through.
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
}

/// What `--dismiss-surveys`' guarded `0` did ([`Session::dismiss_survey`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dismiss {
    /// The `0` was written while the survey was up; `seq` is the server's
    /// stamp (the screen read before it, when it stamped none).
    Pressed { seq: u64 },
    /// Nothing was written: no row matched at the check — the survey had
    /// left the screen first, or its row did not match (the next look finds
    /// it still open and hands it over).
    Skipped,
    /// The host has no `key if=`: nothing is pressed unguarded, and the
    /// survey is said as `EVENT survey …` instead.
    Unguarded,
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
            stray: None,
            survey_gone: false,
            context_warn: 0,
            context: Context::Armed,
            mail: None,
            mail_step: WAIT_STEP,
            hold_step: WAIT_STEP,
        }
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
    pub fn set_mail_step(&mut self, step: Duration) {
        self.mail_step = step;
    }

    /// How long a held idle point (`--mail`) waits for the worker's report
    /// between looks at the screen (the loop's 20 s step unless set; a test
    /// shrinks it) — the safety net under the hold.
    pub fn set_hold_step(&mut self, step: Duration) {
        self.hold_step = step;
    }

    /// The features the server turned out to have (for diagnostics).
    pub fn caps(&self) -> Caps {
        self.caps
    }

    /// One request to the target session; a client that could not be
    /// launched at all is final. Keeps the outage's books: a request the
    /// server did not serve ([`Self::unserved`]) starts one, unless one is in
    /// progress; a request of the outage's kind served again ends it, and so
    /// does an `await seq` that latched — the content moved on the instance
    /// that answers now. The probe read's answer ends nothing.
    pub(super) fn call(&mut self, args: &[&str]) -> Result<CtlReply, Fail> {
        let mut full: Vec<&str> = Vec::with_capacity(args.len() + 1);
        if let Some(sid) = &self.sid {
            full.push(sid.as_str());
        }
        full.extend_from_slice(args);
        let r = self.ctl.call(&full).map_err(Fail::Hard)?;
        let kind = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        if self.unserved(&r) {
            if self.outage.is_none() {
                self.outage = Some(Outage {
                    since: Instant::now(),
                    kind,
                    told: false,
                    back: false,
                    pause: self.pause,
                });
            }
        } else if !self.probing
            && self
                .outage
                .as_ref()
                .is_some_and(|o| o.kind == kind || (kind == "await seq" && r.ok()))
        {
            self.outage = None;
        }
        Ok(r)
    }

    /// The request was not served: the reply never came or the server turned
    /// the connection away ([`CtlReply::lost`]) — or, while an outage is being
    /// ridden out, the server does not host the session: the instance that
    /// adopts the sessions in a handoff may not host this one YET, and a
    /// request it forwards to the instance it replaces falls through to `ERR
    /// no such session` once that one is gone. Outside an outage `no such
    /// session` is the session gone, and final.
    pub(super) fn unserved(&self, r: &CtlReply) -> bool {
        r.lost() || (self.outage.is_some() && r.is_err("no such session"))
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

    /// [`Self::read_screen`], a request not served told apart; the screen is
    /// kept as the last one read.
    /// One read of the worker's screen over the control socket. Named `screen`, not
    /// `read`: the lock-order census (OB-7) identifies a lock by its NAME —
    /// `read`/`write`/`lock`/`try_*` on a receiver — and excludes nothing, by
    /// design; a method called `read` that is held across another `read` reads as a
    /// re-entrant lock and fails the gate (2026-09-12, `press_one_guarded`).
    fn screen(&mut self) -> Result<Screen, Fail> {
        let screen = self.read_once()?;
        self.survey_gone |= !survey_open(&screen.rows);
        self.last = Some(screen.clone());
        Ok(screen)
    }

    fn read_once(&mut self) -> Result<Screen, Fail> {
        if self.caps.tail != Some(false) {
            let tail = format!("tail={TAIL_ROWS}");
            let r = self.call(&["text", "--json", &tail])?;
            if r.usage_error() {
                self.caps.tail = Some(false);
            } else if r.ok() {
                self.caps.tail = Some(true);
                return parse_text_json(&r.stdout).map_err(Fail::Hard);
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
        parse_text_json(&r.stdout).map_err(Fail::Hard)
    }

    /// One `await <cond> timeout <step>`. A request not served is told apart
    /// before the exit code is read: the client's own deadline also exits 124,
    /// and only the server's `OK timeout` is a step that ran out.
    fn wait(&mut self, cond: &[&str], step: Duration) -> Result<Wait, Fail> {
        let ms = step.as_millis().to_string();
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

    /// The turn the TIMEOUT carries when the budget runs out while an outage
    /// is ridden out: the last screen read, as it read (a busy phase and no
    /// rows when no read ever answered).
    fn spent_turn(&self) -> Turn {
        match &self.last {
            Some(screen) => Turn {
                phase: worker_phase(&screen.rows),
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
    /// The loop: wait for the screen to settle (`await idle 2000`, capped at
    /// 20 s a step); read; done unless busy; else wait for the next content
    /// change (`await seq <seq>`) and go again. Where the server knows `await
    /// gone`, the FIRST wait is the busy footer leaving — the signal that holds
    /// when a thinking worker sits static for seconds — falling back to idle on
    /// an older build. A screen without Claude Code's composer frame (a build,
    /// a script, a REPL) that never held still for the 2 s and shows no
    /// approval box is output still arriving, and counts as busy: its turn
    /// ends when the output pauses. An outage is ridden out
    /// ([`Self::ride_out`], its lines on stderr) and the wait starts again
    /// from a fresh read; a timeout spent in one is the timed-out turn all the
    /// same, on the last screen read (`spent_turn`).
    pub fn await_turn(&mut self, timeout: Duration) -> Result<Turn, String> {
        self.await_turn_to(timeout, &mut std::io::stderr())
    }

    /// [`Self::await_turn`], its `RECONNECT …` lines said to `log`.
    fn await_turn_to(&mut self, timeout: Duration, log: &mut dyn Write) -> Result<Turn, String> {
        let deadline = Instant::now() + timeout;
        let mut gone_first = true;
        let mut moved = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            // `--context-warn` is the loops' (`drive` arms it): `await-turn`
            // has no context line to say.
            match self.await_turn_from(remaining, gone_first, &mut moved, &mut |_| Ok(())) {
                Ok(turn) => return Ok(turn),
                Err(Fail::Lost(why)) => {
                    let rode = self.ride_out(why, deadline, &mut |line| {
                        log_line(&mut *log, line);
                        Ok(())
                    })?;
                    if let Rode::Spent = rode {
                        return Ok(self.spent_turn());
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
    /// signal — the settle wait is the first one. Sets `saw_busy` when a read
    /// on the way was busy (the worker moved since the last look); the caller
    /// owns the flag, so what was read before a lost connection is not lost
    /// with it. Every read before the deadline is shown to the context
    /// indicator's watch ([`Self::watch_context`]), a busy one included — a
    /// worker compacts in the middle of a turn — and what it has to say goes
    /// to `say` as the read comes; a read at or after the deadline is not
    /// shown to it, so it says nothing then.
    fn await_turn_from(
        &mut self,
        timeout: Duration,
        gone_first: bool,
        saw_busy: &mut bool,
        say: &mut dyn FnMut(&str) -> Result<(), String>,
    ) -> Result<Turn, Fail> {
        let deadline = Instant::now() + timeout;
        let mut first = gone_first;
        loop {
            let step = deadline
                .saturating_duration_since(Instant::now())
                .min(WAIT_STEP);
            // The first wait was the busy footer leaving, which says nothing
            // about the screen holding still.
            let mut gone = false;
            // The screen held still for IDLE_MS right before the read.
            let mut settled = false;
            if first && self.caps.gone != Some(false) {
                match self.wait(&["gone", BUSY_FOOTER], step)? {
                    Wait::Unsupported => self.caps.gone = Some(false),
                    _ => {
                        self.caps.gone = Some(true);
                        gone = true;
                    }
                }
            }
            if !gone {
                settled = matches!(self.wait(&["idle", IDLE_MS], step)?, Wait::Latched);
            }
            first = false;
            let screen = self.screen()?;
            if Instant::now() < deadline {
                self.watch_context(&screen, say)?;
            }
            let mut phase = worker_phase(&screen.rows);
            // No composer frame and the screen never held still: a build's or a
            // REPL's output still arriving, not the end of anything.
            let writing = !settled
                && !matches!(phase, Phase::Busy | Phase::Prompt)
                && !has_composer_frame(&screen.rows);
            if phase != Phase::Busy && !writing {
                return Ok(Turn {
                    phase,
                    screen,
                    timed_out: false,
                });
            }
            if writing {
                phase = Phase::Busy;
            }
            if !(writing && gone) {
                *saw_busy = true;
                // `--mail`: the turn has begun by now (`MailIn::busy_at`).
                if let Some(m) = &mut self.mail
                    && m.busy_at.is_none()
                {
                    m.busy_at = Some(Instant::now());
                }
            }
            if Instant::now() >= deadline {
                return Ok(Turn {
                    phase,
                    screen,
                    timed_out: true,
                });
            }
            if writing && gone {
                // Settle first: the next wait is the idle one.
                continue;
            }
            let seq = screen.seq.to_string();
            self.wait(&["seq", &seq], step)?;
        }
    }

    /// `--context-warn` at one read: Claude Code's context indicator
    /// ([`context_left`]) against the threshold. The first reading at or
    /// below it says `EVENT context seq=<n> <v>% until auto-compact` — the
    /// worker is about to compact, and a compaction replaces its history with
    /// a summary, so this is the manager's moment to have it bring its
    /// handoff notes up to date — and nothing more is said while it keeps
    /// descending. After that, a read of the composer frame with no indicator
    /// on it, or with one that reads [`COMPACTED_RISE`] points or more over
    /// the last reading, is the compaction: `EVENT compacted seq=<n>` is said
    /// once — the standing rules the manager gave may be gone with the
    /// history — and the watch is armed again, so the next descent is said
    /// too. A read without the frame (not Claude Code, or covered) shows
    /// nothing either way, and one with a box up (the box stands where the
    /// indicator is parked) is no sign that it has gone — a reading on it
    /// still counts. Off at `0`: nothing is said, and no line either loop
    /// prints changes.
    fn watch_context(
        &mut self,
        screen: &Screen,
        say: &mut dyn FnMut(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        let warn = self.context_warn;
        if warn == 0 {
            return Ok(());
        }
        let rows = &screen.rows;
        let left = context_left(rows);
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
                    None => has_composer_frame(rows) && parse_prompt(rows).is_none(),
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
            Ok(End::Stopped(turn, fold)) => Ok((render_result_mail(&turn, &allow, fold), 0)),
            Ok(End::Timeout(turn)) => {
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .record("TIMEOUT", None);
                Ok((
                    format!("TIMEOUT\n{}", render_result(&turn, &allow)),
                    EXIT_TIMEOUT,
                ))
            }
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
    /// looks the same as the one last reported ([`review_key`]: the same phase,
    /// summary and status row, the same last rows of the transcript up to it,
    /// the same box) is neither pressed nor printed again unless a read in
    /// between saw the worker busy, the loop approved a read, or an outage
    /// came in between — so a footer that ticks does not repeat an EVENT,
    /// while a new box, a new reply, a manager's row or a retry's new notice
    /// does, however short the busy spell before it; and since nothing could
    /// be read in an outage, the point still showing after one is reported
    /// once more. An approval prints `APPROVED seq=<n> <command>`. An outage
    /// prints `RECONNECT <reason>`, is ridden out ([`Self::ride_out`]) and
    /// prints `RECONNECTED after <ms> ms` once a read answers again — each
    /// once an outage, however often it comes back before the loop gets past
    /// it. The last line is `TIMEOUT` (returns [`EXIT_TIMEOUT`]) when the
    /// budget is spent — no press and no EVENT comes after the deadline, and a
    /// budget spent in an outage is the TIMEOUT too — or `EXIT <reason>`
    /// (returns 1) when the session goes, the outage outlasts its reconnect
    /// window (`EXIT reconnect window lapsed: <the last failure>`) or the loop
    /// fails (a request, the notes file). Every line is flushed as it is
    /// written. With [`SuperviseOpts::report`], an idle, question or limited
    /// point's line carries `complete=<0|1> rows=<n>` of the report read at
    /// that point ([`reported_event_line`]); without it the line is
    /// [`event_line`]'s. With [`SuperviseOpts::context_warn`] above 0, Claude
    /// Code's context indicator is watched on every read of a turn, a busy
    /// one included: `EVENT context seq=<n> <v>% until auto-compact` when it
    /// first reads at or below the threshold, once a descent, then `EVENT
    /// compacted seq=<n>` once it has gone (or jumped back up) — the worker
    /// compacted, and the watch is armed again.
    pub fn watch(&mut self, opts: &SuperviseOpts, out: &mut (dyn Write + Send)) -> u8 {
        self.watch_to(opts, out, &mut std::io::stderr())
    }

    /// [`Self::watch`] with `--mail`'s lane (`lane`: the second client the
    /// lane parks on the manager's inbox; `None` runs without one, as
    /// [`Self::watch`] does). The lane prints `MAIL id=<n> off=<o> from=<sid>
    /// kind=<k> len=<n> [re=<o>]` per delivery as it lands, on stdout with
    /// every other line; an idle point is held for the worker's report
    /// (`Session::hold_for_report`) and printed as `EVENT turn seq=<n>
    /// report=<id> rows=<n> <summary>` — one line per worker turn, the body
    /// a `inbox get <id>` away — or `EVENT idle-no-report seq=<n> <summary>`
    /// once the grace is spent. A lane that cannot go on says `MAIL lane
    /// off: <why> (the loop goes on without mail)` once, and from then on the
    /// lines are as without the flag.
    pub fn watch_mail<L: Ctl + Send>(
        &mut self,
        opts: &SuperviseOpts,
        lane: Option<&mut L>,
        out: &mut (dyn Write + Send),
    ) -> u8 {
        self.watch_with(opts, lane, out, &mut std::io::stderr())
    }

    /// [`Self::watch`], a journal's one warning said to `warn`. With
    /// `--journal`, every line — the last one too — is appended to the file
    /// before it is printed.
    fn watch_to(
        &mut self,
        opts: &SuperviseOpts,
        out: &mut (dyn Write + Send),
        warn: &mut (dyn Write + Send),
    ) -> u8 {
        self.watch_with(opts, None::<&mut NoLane>, out, warn)
    }

    /// [`Self::watch_to`] with the mail lane.
    fn watch_with<L: Ctl + Send>(
        &mut self,
        opts: &SuperviseOpts,
        lane: Option<&mut L>,
        out: &mut (dyn Write + Send),
        warn: &mut (dyn Write + Send),
    ) -> u8 {
        let allow = opts.allow();
        let journal = Journal::open(opts.journal.as_deref(), self.sid.as_deref(), warn);
        let sink = Mutex::new(Sink {
            out,
            journal,
            warn: Some(warn),
        });
        let end = self.run_loop(
            opts,
            &sink,
            lane,
            &mut Lines {
                sink: &sink,
                allow: &allow,
            },
        );
        let (line, code) = match end {
            Ok(End::Timeout(_)) => ("TIMEOUT".to_string(), EXIT_TIMEOUT),
            // `Lines` never stops at a review point; kept for the match.
            Ok(End::Stopped(..)) => ("EXIT stopped at a review point".to_string(), 1),
            Err(e) => (format!("EXIT {}", exit_reason(&e)), 1),
        };
        // Nothing is left to report a failed write to.
        let _ = sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .line(&line, None);
        code
    }

    /// The loop `supervise` and `watch` share: await the turn; with
    /// `--auto-reads`, approve a read-only Bash prompt ([`Self::auto_read`]);
    /// anything else is a review point for `review`, which stops the loop or
    /// reports the point and keeps watching — and then the loop waits for the
    /// screen to move past it before the next look. A request not served is
    /// ridden out ([`Self::ride_out`], its lines said through `review`) and
    /// the loop looks again from a fresh read; the budget spent in it is the
    /// loop's TIMEOUT. A look that gets through ends the outage.
    fn drive(&mut self, opts: &SuperviseOpts, review: &mut dyn Review) -> Result<End, String> {
        let deadline = Instant::now() + opts.max;
        let allow = opts.allow();
        // Every read of this loop's turns is shown to the context watch
        // (`await_turn_from`), armed from the start.
        self.context_warn = opts.context_warn;
        self.context = Context::Armed;
        let mut state = Looking {
            approved: Vec::new(),
            gone_first: true,
            handed: None,
            moved: false,
            survey: Survey::Closed,
            carried: None,
        };
        loop {
            match self.look(opts, &allow, &mut state, deadline, review) {
                Ok(Some(end)) => return Ok(end),
                Ok(None) => self.outage = None,
                Err(Fail::Lost(why)) => {
                    match self.ride_out(why, deadline, &mut |line| review.say(line))? {
                        Rode::Spent => return Ok(End::Timeout(self.spent_turn())),
                        // Nothing could be read while the session was out of
                        // reach, so nothing says the worker did not move: the
                        // point handed before is handed again if it is still
                        // showing — once an outage, not on every relapse of
                        // it, so an outage that keeps coming back does not
                        // repeat an EVENT each time.
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

    /// One look of [`Self::drive`]: await the turn, then approve a read, hand
    /// a review point over, or pass over the point already handed; `Some`
    /// ends the loop. First, if the fallback press may have left a `1` in the
    /// composer unchecked ([`Self::stray`]), the turn is checked for it: one
    /// found is backspaced and noted, and the loop looks again. Then the
    /// session survey ([`Self::survey`]): one that just appeared is said
    /// before the point — or, `--dismiss-surveys`, dismissed with a guarded
    /// `0`, and the loop looks again from a fresh read, where what the `0`
    /// did is said. The context watch's lines (`--context-warn`,
    /// [`Self::watch_context`]) are said as the turn's reads come, ahead of
    /// all of it.
    fn look(
        &mut self,
        opts: &SuperviseOpts,
        allow: &[String],
        state: &mut Looking,
        deadline: Instant,
        review: &mut dyn Review,
    ) -> Result<Option<End>, Fail> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        // The context watch speaks as the reads come, mid-turn included.
        let mut say = |line: &str| review.say(line);
        // A screen the hold read past its point still showing a point is
        // this look's turn; one busy again is awaited as any turn is.
        let carried = state
            .carried
            .take()
            .filter(|s| worker_phase(&s.rows) != Phase::Busy && has_composer_frame(&s.rows));
        let turn = match carried {
            Some(screen) => Turn {
                phase: worker_phase(&screen.rows),
                screen,
                timed_out: false,
            },
            None => {
                self.await_turn_from(remaining, state.gone_first, &mut state.moved, &mut say)?
            }
        };
        if std::mem::take(&mut state.moved) {
            state.handed = None;
        }
        // The budget bounds every action: a turn read at or after the
        // deadline is neither pressed nor reported — it is the TIMEOUT.
        if turn.timed_out || Instant::now() >= deadline {
            return Ok(Some(End::Timeout(turn)));
        }
        if let Some(command) = self.stray.take()
            && stray_digit(&turn.screen, "1")
        {
            let r = self.call(&["key", "backspace"])?;
            if self.unserved(&r) {
                self.stray = Some(command);
                return Err(Fail::Lost(format!(
                    "key backspace failed: {}",
                    r.stderr.trim()
                )));
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
        if self.survey(&turn, opts, state, review)? {
            state.gone_first = false;
            return Ok(None);
        }
        let seen = if state.handed.as_deref() == Some(review_key(&turn, allow).as_str()) {
            // The point already handed over, still showing: the screen
            // moved (a footer tick, a banner) and nothing the point is made
            // of did. Nothing is pressed or reported.
            turn
        } else {
            let point =
                match self.auto_read(turn, opts, allow, &mut state.approved, deadline, review)? {
                    Step::Again { settle } => {
                        state.handed = None;
                        state.gone_first = !settle;
                        return Ok(None);
                    }
                    Step::Review(point) => point,
                };
            // A point decided at or after the deadline — the press's wait for
            // the box to leave ran out with the budget — is the TIMEOUT too.
            if Instant::now() >= deadline {
                return Ok(Some(End::Timeout(point)));
            }
            // `--mail`: an idle point waits for the worker's report of this
            // turn first; a folded one reads no report from the screen (the
            // mail IS what was said).
            let Held { fold, carried } = if point.phase == Phase::Idle {
                self.hold_for_report(&point, allow, deadline)?
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
            if !review.review(&point, report, fold)? {
                return Ok(Some(End::Stopped(point, fold)));
            }
            // The manager has the worker now, as after a fresh `supervise`.
            state.approved.clear();
            state.handed = Some(review_key(&point, allow));
            // `--mail`: a turn ended here; the next begins after it.
            if let Some(m) = &mut self.mail
                && reported(&point.phase)
            {
                m.floor = Instant::now();
                m.busy_at = None;
                m.handed = true;
            }
            if carried.is_some() {
                // The screen has moved past the point already, and was read.
                state.carried = carried;
                state.gone_first = false;
                return Ok(None);
            }
            point
        };
        if !self.wait_past(seen.screen.seq, deadline)? {
            return Ok(Some(End::Timeout(seen)));
        }
        state.gone_first = false;
        Ok(None)
    }

    /// The session survey at one look ([`survey_open`]). On the look where it
    /// APPEARS — open now, and not said since a read last showed it gone
    /// ([`Survey::Closed`]) — the loop says `EVENT survey seq=<n> dismiss
    /// with: aterm ctl @<sid> key 'if=^●.How.is.Claude.doing' 0`
    /// ([`survey_event_line`]). A survey that stays open is said once; one
    /// that any read shows gone — a busy read on the way to the turn included
    /// ([`Session::survey_gone`]) — and then open again has appeared again.
    ///
    /// With `--dismiss-surveys` the loop presses that `0` itself
    /// ([`Self::dismiss_survey`]) and `true` has it look again from a fresh
    /// read, where the `0` is judged ([`Survey::Tried`]): the survey gone,
    /// the loop says `DISMISSED survey seq=<n>` (the press's stamp; noted in
    /// `--notes` too), or nothing when the press was skipped; the survey
    /// still open — the `0` did not take, or the guard matched no row of it —
    /// is handed over with the `EVENT survey` line (and a note), never
    /// pressed again; a `0` that landed in the composer instead is backspaced
    /// and noted, nothing is said dismissed, and a survey still open then is
    /// handed over the same way.
    ///
    /// While a box is up (`prompt`) or text is typed in the composer, the
    /// survey waits — nothing is said or pressed: the box is handled first,
    /// and a `0` would land in the draft — so a survey that appeared under a
    /// box is said (or dismissed) at the first look without one; such a look
    /// only settles a survey that has gone. A press whose answer never came
    /// is ridden out with the survey not tried yet, so it is tried again from
    /// the next read.
    fn survey(
        &mut self,
        turn: &Turn,
        opts: &SuperviseOpts,
        state: &mut Looking,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let open = survey_open(&turn.screen.rows);
        // The survey the last look left is gone if any read since showed
        // none open (this turn's read among them).
        let left = std::mem::take(&mut self.survey_gone);
        if let Survey::Tried { pressed: Some(_) } = state.survey
            && stray_digit(&turn.screen, "0")
        {
            let r = self.call(&["key", "backspace"])?;
            if self.unserved(&r) {
                self.survey_gone |= left;
                return Err(Fail::Lost(format!(
                    "key backspace failed: {}",
                    r.stderr.trim()
                )));
            }
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
            state.survey = Survey::Closed;
            if open {
                // Handed over, never pressed again while it stays open.
                state.survey = Survey::Said;
                review.say(&survey_event_line(turn.screen.seq, self.sid.as_deref()))?;
            }
            return Ok(true);
        }
        let waits = turn.phase == Phase::Prompt || typed_draft(&turn.screen);
        match state.survey {
            Survey::Tried { pressed } if left => {
                if let Some(seq) = pressed {
                    append_note(
                        opts.notes.as_deref(),
                        "dismissed the session survey (pressed 0; a rating is the human's)",
                    )?;
                    review.say(&format!("DISMISSED survey seq={seq}"))?;
                }
                state.survey = Survey::Closed;
            }
            // Still open under a box or a draft: judged at the first look
            // without them.
            Survey::Tried { .. } if waits => return Ok(false),
            Survey::Tried { pressed } => {
                let why = if pressed.is_some() {
                    "still open after its guarded 0"
                } else {
                    "its guarded 0 matched no row, and it is still open"
                };
                self.hand_survey(turn, opts, state, review, why)?;
                return Ok(false);
            }
            Survey::Said if left => state.survey = Survey::Closed,
            Survey::Said | Survey::Closed => {}
        }
        if !open || waits || state.survey == Survey::Said {
            return Ok(false);
        }
        if opts.dismiss_surveys {
            match self.dismiss_survey(turn.screen.seq)? {
                Dismiss::Pressed { seq } => {
                    state.survey = Survey::Tried { pressed: Some(seq) };
                    return Ok(true);
                }
                Dismiss::Skipped => {
                    state.survey = Survey::Tried { pressed: None };
                    return Ok(true);
                }
                Dismiss::Unguarded => {}
            }
        }
        state.survey = Survey::Said;
        review.say(&survey_event_line(turn.screen.seq, self.sid.as_deref()))?;
        Ok(false)
    }

    /// Hand the open session survey to the manager when `--dismiss-surveys`'
    /// `0` did not dismiss it: `why` noted, the `EVENT survey` line said, and
    /// nothing pressed on it again while it stays open.
    fn hand_survey(
        &mut self,
        turn: &Turn,
        opts: &SuperviseOpts,
        state: &mut Looking,
        review: &mut dyn Review,
        why: &str,
    ) -> Result<(), Fail> {
        append_note(
            opts.notes.as_deref(),
            &format!("handed the session survey to the manager ({why})"),
        )?;
        state.survey = Survey::Said;
        review.say(&survey_event_line(turn.screen.seq, self.sid.as_deref()))?;
        Ok(())
    }

    /// `--dismiss-surveys`' press: `key if=^●.How.is.Claude.doing 0`, the
    /// check and the press under one server lock, as the approval press
    /// guards its `1` — `OK skipped seq=<n>` means no row matched and nothing
    /// was written, so a survey that left first never gets a `0` in the
    /// composer (a copy quoted in the transcript does not match:
    /// [`SURVEY_ROW`]).
    /// `0` is the only key it ever presses. A host without the guard (a usage
    /// line or a bare `ERR`) is never pressed unguarded: [`Dismiss::Unguarded`],
    /// and the survey is said instead. `ERR busy sink` is retried as the
    /// approval press retries it; a request not served is ridden out; any
    /// other `ERR` ends the loop. `seen` is the seq of the screen read before.
    fn dismiss_survey(&mut self, seen: u64) -> Result<Dismiss, Fail> {
        if self.caps.key_if == Some(false) {
            return Ok(Dismiss::Unguarded);
        }
        let cond = format!("if={SURVEY_ROW}");
        let mut busy = 0;
        loop {
            let r = self.call(&["key", &cond, "0"])?;
            if r.ok() {
                self.caps.key_if = Some(true);
                return Ok(if r.skipped() {
                    Dismiss::Skipped
                } else {
                    Dismiss::Pressed {
                        seq: r.seq().unwrap_or(seen),
                    }
                });
            }
            let why = format!("key {cond} 0 failed: {}", r.stderr.trim());
            if self.unserved(&r) {
                return Err(Fail::Lost(why));
            }
            if r.unknown_form() {
                self.caps.key_if = Some(false);
                return Ok(Dismiss::Unguarded);
            }
            if r.is_err("busy sink") && busy < BUSY_SINK_RETRIES {
                busy += 1;
                std::thread::sleep(BUSY_SINK_BACKOFF);
                continue;
            }
            return Err(Fail::Hard(why));
        }
    }

    /// `watch --report`'s count of the report at a point. A request not served
    /// is ridden out like any other; a report that fails otherwise (a reply
    /// that does not add up) does not end the loop — the point is reported
    /// `complete=0 rows=0` and the failure noted — since the EVENT is what the
    /// manager is woken for, and a session that is gone ends the loop at its
    /// next request anyway.
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
    /// when there is no lane (no `--mail`, or the lane ended). The worker's
    /// `report`s come from the lane as they land ([`Delivery`]); the newest
    /// pending one that is this turn's ([`MailIn::is_this_turns`]) folds at
    /// once — the rest are forgotten, an earlier turn's — and none pending,
    /// the point is HELD until one lands (a fold) or the grace runs out
    /// ([`Fold::NoReport`]), the budget bounding the hold as it bounds every
    /// wait. Claude Code's Stop hook posts the report as the turn ends, a
    /// moment before or after the screen settles, so the hold is short where
    /// there is a hook and the grace where there is none.
    ///
    /// THE SCREEN IS THE SAFETY NET UNDER THE HOLD: once a `hold_step` (20 s)
    /// of it has passed with no report, the screen is read once — a footer
    /// that ticked is the same point ([`review_key`]) and the hold goes on;
    /// a prompt, a question or the worker busy again has superseded the
    /// point, which is said as `idle-no-report` (the turn did end, and no
    /// report came before the worker moved on) with the screen read carried
    /// to the next look ([`Held::carried`]). Without it a prompt right after
    /// an idle — a queued message, a wake — waited the whole grace unseen.
    fn hold_for_report(
        &mut self,
        point: &Turn,
        allow: &[String],
        deadline: Instant,
    ) -> Result<Held, Fail> {
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
        let key = review_key(point, allow);
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
                    let now_turn = Turn {
                        phase: worker_phase(&screen.rows),
                        screen,
                        timed_out: false,
                    };
                    if review_key(&now_turn, allow) == key {
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

    /// Ride out an outage — a request the server did not serve
    /// ([`Self::unserved`]), as when an aterm self-update hands every session
    /// to the new instance under the same `@sid`. Reads the screen of the same
    /// selector after a pause that starts at 0.5 s and doubles up to 8 s
    /// across the outage, until a read answers
    /// (`Rode::Back`) or the window runs out. The client resolves its socket
    /// afresh for every request: with none named (no `--socket`, no
    /// `$ATERM_CONTROL_SOCK`) that is the instance hosting the caller's own
    /// terminal, else the newest instance (the `aterm.sock` alias) — after a
    /// handoff, the new instance, which hosts the sid or forwards the request
    /// to the instance that does. A socket named is dialed as named, and a
    /// per-instance one (`aterm-<pid>.sock`) goes with its instance, so an
    /// outage through one lapses.
    ///
    /// The window is the OUTAGE's, not one ride-out's: it runs from the first
    /// request not served, and an outage lasts until the loop gets past it — a
    /// request of the kind that failed is served again ([`Self::call`]), an
    /// `await seq` latches, or a look gets through ([`Self::drive`]). A read
    /// answering here says only that the session answers a read, so a request
    /// not served after it, before any of those, is the same outage: its
    /// window runs on, and nothing more is said. `RECONNECT <why>` (cut at 160
    /// characters) is said at an outage's first ride-out, and `RECONNECTED
    /// after <ms> ms` (since the outage began) at its first answer.
    ///
    /// Ends the loop (`Err`) when the window runs out (`reconnect window
    /// lapsed: <the last failure>`), with `why` as it came when the window is
    /// zero (`--reconnect-s 0`), and at once when the server answers a probe
    /// with an `ERR` that [`Self::unserved`] does not name (`ERR exited`, …).
    /// The budget spent first — before the ride-out, or during it — is
    /// `Rode::Spent`, the loop's TIMEOUT. The read is the probe, never the
    /// request that failed: an `await seq <n>` from before a handoff names a
    /// count the new instance has not reached (its content seq starts over —
    /// measured, from about 652 to 12), and a press is decided again from a
    /// fresh read. The probed caps are forgotten first: the instance that
    /// answers may be another build.
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

    /// `--auto-reads` on one turn: a Bash prompt whose command classifies
    /// read-only is approved with the guarded press (noted, and reported to
    /// `review`), and the loop looks again once the box has LEFT; anything
    /// else — not a prompt, not Bash, not read-only, the same read back after
    /// two approvals, a guard that matched no row of this very box, a box that
    /// did not move after the press, a fallback press withheld because the
    /// session survey is open ([`Pressing::Withheld`]) — is a review point.
    /// When the connection is lost before the press is seen through, what the
    /// loop knows it did is noted — a `1` the server confirmed is an approval,
    /// a press whose answer never came is noted as such and is not — and the
    /// loss is ridden out.
    fn auto_read(
        &mut self,
        turn: Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        approved: &mut Vec<String>,
        deadline: Instant,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let bash = (opts.auto_reads && turn.phase == Phase::Prompt)
            .then(|| parse_prompt(&turn.screen.rows))
            .flatten()
            .filter(|p| p.kind == PromptKind::Bash);
        let Some(p) = bash else {
            return Ok(Step::Review(turn));
        };
        let v = classify_command_with(&p.command, allow);
        let repeats = approved.iter().filter(|c| **c == p.command).count();
        let handed = if !v.read_only {
            Some(v.reason.clone())
        } else if repeats >= MAX_APPROVALS_OF_ONE_COMMAND {
            Some("the same prompt came back after two approvals".to_string())
        } else {
            None
        };
        if let Some(why) = handed {
            append_note(
                opts.notes.as_deref(),
                &format!("handed to the manager ({why}): {}", p.command),
            )?;
            return Ok(Step::Review(turn));
        }
        let mut approve = |seq: u64| -> Result<(), String> {
            approved.push(p.command.clone());
            append_note(
                opts.notes.as_deref(),
                &format!("approved read-only: {}", p.command),
            )?;
            review.approved(seq, &p.command)
        };
        let press = match self.press_one_guarded(&p, &turn.screen)? {
            Pressing::Done(press) => press,
            Pressing::Withheld => {
                append_note(
                    opts.notes.as_deref(),
                    &format!(
                        "handed to the manager (the session survey is open and this host has \
                         no guarded press: an unguarded 1 could rate the session): {}",
                        p.command
                    ),
                )?;
                return Ok(Step::Review(turn));
            }
            Pressing::Lost { known, why } => {
                match known {
                    Some(Press::Pressed { seq }) => approve(seq)?,
                    Some(Press::Skipped { .. }) => append_note(
                        opts.notes.as_deref(),
                        &format!(
                            "nothing pressed, the box had left the screen: {}",
                            p.command
                        ),
                    )?,
                    None => append_note(
                        opts.notes.as_deref(),
                        &format!(
                            "pressed, no answer came; the box is read again after the \
                             reconnect: {}",
                            p.command
                        ),
                    )?,
                }
                return Err(Fail::Lost(why));
            }
        };
        match press {
            Press::Pressed { seq } => {
                approve(seq)?;
                // The box must LEAVE before the next look: `await gone` is
                // level-triggered and a box shows no busy footer, so an
                // immediate re-read of an unchanged screen would match the same
                // box and press it again — the stray digit the guard exists to
                // prevent.
                let screen = match self.moved_past(seq, deadline)? {
                    Past::Moved => return Ok(Step::Again { settle: true }),
                    Past::Still(Some(screen)) => screen,
                    // The budget ran out waiting: nothing is read or handed
                    // over after it — the box as last read is the TIMEOUT's
                    // ([`Self::look`]).
                    Past::Still(None) => return Ok(Step::Review(turn)),
                };
                append_note(
                    opts.notes.as_deref(),
                    &format!(
                        "handed to the manager (the box did not change after the press): {}",
                        p.command
                    ),
                )?;
                Ok(Step::Review(turn_of(screen)))
            }
            Press::Skipped { seq } => {
                if seq == turn.screen.seq {
                    // The very screen we parsed, and the guard found no row:
                    // this box is not one the supervisor answers.
                    append_note(
                        opts.notes.as_deref(),
                        &format!(
                            "handed to the manager (the guarded press matched no row): {}",
                            p.command
                        ),
                    )?;
                    return Ok(Step::Review(turn));
                }
                append_note(
                    opts.notes.as_deref(),
                    &format!(
                        "nothing pressed, the box had left the screen: {}",
                        p.command
                    ),
                )?;
                Ok(Step::Again { settle: false })
            }
        }
    }

    /// Wait for the content to move past `seq`, step after step, until
    /// `deadline`: `false` when the budget ran out with the screen unchanged.
    fn wait_past(&mut self, seq: u64, deadline: Instant) -> Result<bool, Fail> {
        while Instant::now() < deadline {
            if let Past::Moved = self.moved_past(seq, deadline)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Wait (one step, capped by `deadline`) for the content to move past
    /// `seq`. A step that runs out is checked with a read: `await seq <n>`
    /// latches only once THIS instance's count passes `n`, so after a handoff
    /// no request saw fail — the instance handing over answered the step (its
    /// readers parked, its count frozen), or the step ended as the successor
    /// took the socket — the loop would wait on a count the successor, whose
    /// count started over, may not reach for hours. A read BELOW `seq` is
    /// that other count: the screen moved. Nothing is read once the budget is
    /// spent.
    fn moved_past(&mut self, seq: u64, deadline: Instant) -> Result<Past, Fail> {
        let step = deadline
            .saturating_duration_since(Instant::now())
            .min(WAIT_STEP);
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

    /// Press option 1 so that a prompt that resolved meanwhile never gets a
    /// stray `1` in the composer: `key if=Do.you.want.to.proceed 1` where the
    /// server has it (the check and the press happen under one lock; `OK
    /// skipped seq=<n>` means no row matched and nothing was written), else
    /// read → confirm the same box is still up → press → wait for the worker
    /// to take it → re-read → backspace a digit that landed in the composer.
    /// The fallback presses nothing while the session survey is open on the
    /// confirming read ([`Pressing::Withheld`]): a `1` that reaches the
    /// survey instead of the box is a rating, and no digit is left to find.
    ///
    /// The capability probe is the reply: `OK …` proves the guard (pressed or
    /// skipped); a usage line or a bare `ERR` — what a build without `if=`
    /// answers, reading `if=…` as a key name — falls back for good; `ERR busy
    /// sink` is retried; every other `ERR` (`halted`, `no such session`,
    /// `badregex`) is an error, never an approval.
    ///
    /// A lost connection is [`Pressing::Lost`]: a press whose answer never
    /// came is not an approval, and is not sent again as it was — the loop
    /// rides the connection out, then reads and classifies again
    /// ([`Self::drive`]), the box may have moved or gone. On the fallback path
    /// a `1` the server confirmed is written, lost connection or not: it is
    /// the approval it was, and what could not be checked after it — a digit
    /// in the composer — is checked on the next turn ([`Self::stray`]), as it
    /// is after a fallback press whose answer never came.
    fn press_one_guarded(&mut self, prompt: &Prompt, seen: &Screen) -> Result<Pressing, Fail> {
        if self.caps.key_if != Some(false) {
            let cond = format!("if={PROCEED}");
            let mut busy = 0;
            loop {
                let r = self.call(&["key", &cond, "1"])?;
                if r.ok() {
                    self.caps.key_if = Some(true);
                    let seq = r.seq().unwrap_or(seen.seq);
                    return Ok(Pressing::Done(if r.skipped() {
                        Press::Skipped { seq }
                    } else {
                        Press::Pressed { seq }
                    }));
                }
                let why = format!("key {cond} 1 failed: {}", r.stderr.trim());
                if self.unserved(&r) {
                    // The guard ran under the server's lock or not at all: a
                    // `1` it wrote went to the box, never the composer.
                    return Ok(Pressing::Lost { known: None, why });
                }
                if r.unknown_form() {
                    self.caps.key_if = Some(false);
                    break;
                }
                if r.is_err("busy sink") && busy < BUSY_SINK_RETRIES {
                    busy += 1;
                    std::thread::sleep(BUSY_SINK_BACKOFF);
                    continue;
                }
                return Err(Fail::Hard(why));
            }
        }
        let now = self.screen()?;
        match parse_prompt(&now.rows) {
            Some(q) if q.command == prompt.command => {}
            _ => return Ok(Pressing::Done(Press::Skipped { seq: now.seq })),
        }
        // An unguarded `1` goes wherever keys go when it lands: to the box,
        // or — the box resolved first — to the session survey parked above
        // the composer, where it is the rating `Bad`, the human's to give, and
        // leaves no digit in the composer to find. With the survey open,
        // nothing is pressed: the box is the manager's.
        if survey_open(&now.rows) {
            return Ok(Pressing::Withheld);
        }
        let r = self.call(&["key", "1"])?;
        if !r.ok() {
            let why = format!("key 1 failed: {}", r.stderr.trim());
            if self.unserved(&r) {
                self.stray = Some(prompt.command.clone());
                return Ok(Pressing::Lost { known: None, why });
            }
            return Err(Fail::Hard(why));
        }
        let seq = r.seq().unwrap_or(now.seq);
        let pressed = Press::Pressed { seq };
        // Let the worker take the press before looking for a stray digit: the
        // first content change after it, bounded.
        let after = match self
            .wait(&["seq", &seq.to_string()], STRAY_SETTLE)
            .and_then(|_| self.screen())
        {
            Ok(after) => after,
            Err(Fail::Lost(why)) => {
                self.stray = Some(prompt.command.clone());
                return Ok(Pressing::Lost {
                    known: Some(pressed),
                    why,
                });
            }
            Err(e) => return Err(e),
        };
        if stray_digit(&after, "1") {
            let r = self.call(&["key", "backspace"])?;
            let skipped = Press::Skipped { seq: after.seq };
            if self.unserved(&r) {
                self.stray = Some(prompt.command.clone());
                return Ok(Pressing::Lost {
                    known: Some(skipped),
                    why: format!("key backspace failed: {}", r.stderr.trim()),
                });
            }
            return Ok(Pressing::Done(skipped));
        }
        Ok(Pressing::Done(pressed))
    }
}

/// One screen, classified as a turn that ended.
fn turn_of(screen: Screen) -> Turn {
    Turn {
        phase: worker_phase(&screen.rows),
        screen,
        timed_out: false,
    }
}

/// `digit` sitting alone in the composer with no box on the screen: the
/// digit a press wrote after what it answered had left on its own — the
/// fallback press's `1` after the prompt resolved, `--dismiss-surveys`' `0`
/// after the survey went (typed text, not the placeholder the composer shows
/// when empty).
fn stray_digit(screen: &Screen, digit: &str) -> bool {
    parse_prompt(&screen.rows).is_none()
        && composer_text(&screen.rows).as_deref() == Some(digit)
        && !is_placeholder(&screen.rows, screen.cursor_col)
}

/// Text typed into the composer — not the placeholder suggestion it shows
/// when empty: a `0` pressed now could land in it. A draft can fill more
/// than one row ([`composer_draft`]): text on any row under the caret row is
/// typed (the placeholder is one row), and so is text on the caret row
/// unless the cursor sits at column 2 of THAT row ([`is_placeholder`]'s
/// measure) — a cursor at column 2 of another row of the composer is a
/// draft's, after a line break. A cursor on no row read is judged by its
/// column alone.
fn typed_draft(screen: &Screen) -> bool {
    let Some((caret, lines)) = composer_draft(&screen.rows) else {
        return false;
    };
    if lines.iter().skip(1).any(|l| !l.is_empty()) {
        return true;
    }
    if lines.first().is_none_or(|l| l.is_empty()) {
        return false;
    }
    let on_caret_row = screen.cursor_index().is_none_or(|i| i == caret);
    !(on_caret_row && screen.cursor_col == 2)
}

/// The line `watch` prints (and `supervise` says on stderr) when the session
/// survey appears: `EVENT survey seq=<n> dismiss with: aterm ctl @<sid> key
/// 'if=^●.How.is.Claude.doing' 0` — the guarded `0` that dismisses it, the
/// one key that is the manager's to press there (with no `@sid` named, none
/// is in the command either). The guard is quoted: `^` is a glob operator in
/// a zsh with `extended_glob` set, where the bare word fails as `no matches
/// found`. `EVENT`, so a monitor that wakes on EVENT lines wakes on it: a
/// turn typed while it is open whose first character is 1, 2 or 3 is taken
/// as a rating.
fn survey_event_line(seq: u64, sid: Option<&str>) -> String {
    let target = sid.map(|s| format!(" {s}")).unwrap_or_default();
    format!("EVENT survey seq={seq} dismiss with: aterm ctl{target} key 'if={SURVEY_ROW}' 0")
}

/// What `phase` prints: [`render_phase`]'s lines, then `survey 0` when the
/// session survey is open ([`survey_open`]) — the key that dismisses it. A
/// rating is the human's; `0` is the one key a manager presses on it. Last,
/// `context <n>%` when Claude Code's context indicator is up ([`context_left`]):
/// that much is left before the worker auto-compacts. Without either, the
/// lines are [`render_phase`]'s alone.
pub fn render_phase_and_survey(turn: &Turn, python_allow: &[String]) -> String {
    let mut out = render_phase(turn, python_allow);
    if survey_open(&turn.screen.rows) {
        out.push_str("survey 0\n");
    }
    if let Some(left) = context_left(&turn.screen.rows) {
        out.push_str(&format!("context {left}%\n"));
    }
    out
}

/// The lines `phase` / `await-turn` print: the phase word, then for a prompt
/// its parsed kind, command, description, classification and options; for
/// busy, `reason <zone>: <rule>` (which signal fired, and where — `whole
/// screen, no composer frame: output still changing` when a wait ran out on a
/// screen without the composer frame that kept changing, `no screen: no read
/// answered before the timeout` when the budget ran out in an outage before
/// any read did); for a limit notice, `message <text>` and `reset <text|->`.
pub fn render_phase(turn: &Turn, python_allow: &[String]) -> String {
    let mut out = format!("{}\n", turn.phase.name());
    match &turn.phase {
        Phase::Prompt => {
            if let Some(p) = parse_prompt(&turn.screen.rows) {
                out.push_str(&render_prompt(&p, python_allow));
            }
        }
        Phase::Busy => match busy_signal(&turn.screen.rows) {
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

fn event_summary(turn: &Turn, python_allow: &[String]) -> String {
    match &turn.phase {
        Phase::Prompt => {
            let Some(p) = parse_prompt(&turn.screen.rows) else {
                return "kind=other classify=- command=-".to_string();
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
        Phase::Limited { message, reset } => format!(
            "message={} reset={}",
            clip(message),
            reset.as_deref().map_or_else(|| "-".to_string(), clip)
        ),
        Phase::Busy | Phase::Idle | Phase::Question => {
            or_dash(&clip(last_said_row(&turn.screen.rows).unwrap_or("").trim()))
        }
    }
}

/// What makes two review points the same one — never the seq, which any
/// footer tick moves: the phase, the event summary, the status row, and the
/// words of the last [`KEY_ROWS`] non-blank rows of the transcript, up to the
/// last thing said or, for a prompt, up to its box, whose rows count verbatim.
/// Anything the worker, a tool or the manager adds lands in those rows (a new
/// box comes after the output of the one before it, a retry after its `❯`
/// row), while a footer or a banner does not; letters only, so a timer that
/// ticks or a glyph that blinks does not make a row new.
fn review_key(turn: &Turn, python_allow: &[String]) -> String {
    let rows = &turn.screen.rows;
    let mut key = format!(
        "{} {}\n{}\n",
        turn.phase.name(),
        event_summary(turn, python_allow),
        status_row(rows).unwrap_or("")
    );
    let span = (turn.phase == Phase::Prompt)
        .then(|| prompt_box_span(rows))
        .flatten();
    let upto = match span {
        Some((a, _)) => a,
        None => last_said_index(rows).map_or(0, |k| k + 1),
    };
    let said: Vec<&String> = rows[..upto]
        .iter()
        .filter(|r| !r.trim().is_empty())
        .collect();
    for r in &said[said.len().saturating_sub(KEY_ROWS)..] {
        key.extend(r.chars().filter(|c| c.is_alphabetic()));
        key.push('\n');
    }
    if let Some((a, b)) = span {
        for r in &rows[a..=b] {
            key.push_str(r.trim_end());
            key.push('\n');
        }
    }
    key
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

fn render_prompt(p: &Prompt, python_allow: &[String]) -> String {
    let mut out = format!("kind {}\n", p.kind.name());
    if !p.command.is_empty() {
        out.push_str(&format!("command {}\n", p.command));
    }
    if !p.description.is_empty() && p.kind != PromptKind::Other {
        out.push_str(&format!("description {}\n", p.description));
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
    out.push_str(if p.has_cancel {
        "cancel esc\n"
    } else {
        "cancel none\n"
    });
    out
}

/// [`render_result`] as `supervise --mail` prints it: after the phase lines,
/// `report <id> rows=<n>` for an idle point the worker's report was folded
/// into, `report -` for one no report came for; without a fold, the result
/// as it always was.
pub fn render_result_mail(turn: &Turn, python_allow: &[String], fold: Option<Fold>) -> String {
    let mail = match fold {
        Some(Fold::Report { id, rows }) => format!("report {id} rows={rows}\n"),
        Some(Fold::NoReport) => "report -\n".to_string(),
        None => String::new(),
    };
    let mut out = render_phase(turn, python_allow);
    out.push_str(&mail);
    out.push_str(&render_result_rows(turn));
    out
}

/// The compact result `supervise` prints: the phase lines, then the prompt box
/// verbatim, or the last 28 non-blank rows.
pub fn render_result(turn: &Turn, python_allow: &[String]) -> String {
    let mut out = render_phase(turn, python_allow);
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
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open notes file {}: {e}", path.display()))?;
    writeln!(f, "{} {line}", utc_stamp(secs))
        .map_err(|e| format!("cannot append to notes file {}: {e}", path.display()))
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time (civil-from-days, Howard Hinnant).
pub fn utc_stamp(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::super::prompt::fixtures::{bash_one_row, composer, rows, workflow_box};
    use super::*;
    use std::collections::{BTreeMap, VecDeque};

    /// A scripted server: each `call` pops the next reply and records the
    /// request; screens are served in order from `screens`.
    ///
    /// The model behind the built-in answers: the content sequence advances on
    /// every read; `await seq <n>` latches only while another screen is still
    /// to come (the worker moved on) and TIMES OUT on the last one (the screen
    /// sits unchanged); a modern `key if=` is judged against the screen last
    /// served as the server judges it — the pattern compiled by the server's
    /// own `aterm_observe::row_matcher` and tested against every row: `OK
    /// seq=<n>` when a row matches, else `OK skipped seq=<n>` (`ERR badregex`
    /// for a pattern that does not compile) — and an older host answers it
    /// with the bare `ERR` measured on aterm 0.81.0.
    ///
    /// `await idle` answers at once by default, so every scripted screen is
    /// read. The real server's `await idle 2000` latches only once the content
    /// held still for 2 s, and a turning spinner never does: with
    /// `idle_skips_busy`, an `await idle` passes over the busy screens next in
    /// the script (the content moves, nothing reads them) when another screen
    /// follows them — a busy spell the loop never sees.
    ///
    /// A handoff: `by_index` answers chosen requests (by arrival order) ahead
    /// of every other rule — [`closed`], the connection the old instance
    /// dropped, or a server's `ERR` — and from `down`'s index on every request
    /// gets `down`'s reply (the session never came back, or never serves
    /// again). From `drop_awaits_from` on every `await` is [`closed`] while
    /// reads are served: an outage that keeps coming back. The first lost
    /// reply served starts the content seq over at `handoff_seq`, as the
    /// instance that adopts the session counts from its own start; `restart`
    /// starts it over at a request with none failing — a handoff no request
    /// saw fail. `await seq <n>` for an `n` the content has not reached — a
    /// number from before the handoff — never latches, as on the real server
    /// (it passes the stalls the way the last screen does). `delay` holds a
    /// request's answer back.
    struct Mock {
        requests: Vec<String>,
        /// Replies to non-`text`, non-`key` requests, in order (missing = the
        /// built-in answer).
        replies: VecDeque<CtlReply>,
        /// Scripted replies to `key` requests, in order (missing = the model).
        key_replies: VecDeque<CtlReply>,
        /// Screens to serve to `text` requests, in order (the last repeats).
        screens: Vec<Vec<String>>,
        served: usize,
        /// Whether the server knows `tail=`, `await gone`, `key if=`.
        modern: bool,
        seq: u64,
        /// After this many `await seq` timeouts on the last screen the session
        /// is gone — the wait answers `ERR exited`, as the real server's await
        /// does when the session ends under it — which is how a `watch` test
        /// ends.
        vanish_after: Option<u32>,
        stalls: u32,
        /// `await idle` passes over busy screens (see the struct doc).
        idle_skips_busy: bool,
        /// Replies by request index, ahead of everything (see the struct doc).
        by_index: BTreeMap<usize, CtlReply>,
        /// From this request index on, every request gets this reply.
        down: Option<(usize, CtlReply)>,
        /// From this request index on, every `await` answers [`closed`].
        drop_awaits_from: Option<usize>,
        /// Where the content seq starts over at the first lost reply served.
        handoff_seq: Option<u64>,
        /// At this request index the content seq starts over at this value,
        /// before the request is served.
        restart: Option<(usize, u64)>,
        /// How long the answer to the request at an index is held back.
        delay: BTreeMap<usize, Duration>,
        /// The `history` reply (`None`: a host without the verb).
        history: Option<CtlReply>,
        /// `offscreen` replies, in order (none left: a host without the verb).
        offscreen: VecDeque<CtlReply>,
        /// The mail lane's `await inbox` answers this mock RELEASES as it
        /// serves chosen requests (by arrival index), so a test decides
        /// whether the worker's report lands before or after the idle point
        /// the screen shows ([`MailMock`]).
        release: Option<std::sync::mpsc::Sender<CtlReply>>,
        release_at: BTreeMap<usize, Vec<CtlReply>>,
    }

    fn ok(stdout: &str) -> CtlReply {
        CtlReply {
            code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }
    fn timeout() -> CtlReply {
        CtlReply {
            code: 124,
            stdout: "OK timeout\n".to_string(),
            stderr: String::new(),
        }
    }
    fn usage(verb: &str) -> CtlReply {
        err(&format!("usage: {verb}"))
    }
    fn err(text: &str) -> CtlReply {
        CtlReply {
            code: 1,
            stdout: String::new(),
            stderr: format!("aterm-ctl: ERR {text}\n"),
        }
    }
    /// What aterm 0.81.0 answers `key if=… 1` (and `key nosuchkey`): the
    /// option read as a key name, `aterm-ctl: ERR` and nothing more.
    fn bare_err() -> CtlReply {
        CtlReply {
            code: 1,
            stdout: String::new(),
            stderr: "aterm-ctl: ERR\n".to_string(),
        }
    }
    /// What the client printed when aterm 0.82.0 handed its sessions to
    /// 0.83.0 under a running `watch` (measured 2026-09-12): the connection
    /// closed before any answer came.
    fn closed() -> CtlReply {
        failed(1, "server closed the connection without responding")
    }
    /// A client-side failure: `aterm-ctl: <text>` on stderr, nothing on stdout.
    fn failed(code: i32, text: &str) -> CtlReply {
        CtlReply {
            code,
            stdout: String::new(),
            stderr: format!("aterm-ctl: {text}\n"),
        }
    }

    impl Mock {
        fn new(modern: bool, screens: Vec<Vec<String>>) -> Self {
            Self {
                requests: Vec::new(),
                replies: VecDeque::new(),
                key_replies: VecDeque::new(),
                screens,
                served: 0,
                modern,
                seq: 100,
                vanish_after: None,
                stalls: 0,
                idle_skips_busy: false,
                by_index: BTreeMap::new(),
                down: None,
                drop_awaits_from: None,
                handoff_seq: None,
                restart: None,
                delay: BTreeMap::new(),
                history: None,
                offscreen: VecDeque::new(),
                release: None,
                release_at: BTreeMap::new(),
            }
        }
        fn last_served(&self) -> &[String] {
            &self.screens[self.served.saturating_sub(1).min(self.screens.len() - 1)]
        }
        fn screen_json(&mut self) -> String {
            let i = self.served.min(self.screens.len() - 1);
            self.served += 1;
            self.seq += 1;
            let rows: Vec<String> = self.screens[i]
                .iter()
                .map(|r| format!("\"{}\"", r.replace('\\', "\\\\").replace('"', "\\\"")))
                .collect();
            let composer_col = if self.screens[i].iter().any(|r| r == "❯ 1" || r == "❯ 0") {
                3
            } else {
                2
            };
            format!(
                "{{\"rows\":[{}],\"cursor\":{{\"row\":10,\"col\":{composer_col},\"visible\":true,\"style\":\"block\"}},\"dims\":{{\"rows\":40,\"cols\":120}},\"seq\":{}}}\n",
                rows.join(","),
                self.seq
            )
        }
        fn presses(&self) -> Vec<&String> {
            self.requests
                .iter()
                .filter(|r| r.contains("key "))
                .collect()
        }
    }

    impl Mock {
        /// The built-in answers and the scripted queues (see the struct doc).
        fn serve(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            let line = args.join(" ");
            self.requests.push(line.clone());
            let verb_at = usize::from(args.first().is_some_and(|a| a.starts_with('@')));
            let verb = args.get(verb_at).copied().unwrap_or("");
            let tail = &args[verb_at + 1..];
            match verb {
                "text" => {
                    if tail.iter().any(|a| a.starts_with("tail=")) && !self.modern {
                        return Ok(usage("text [--json] [trim]"));
                    }
                    let body = self.screen_json();
                    Ok(ok(&body))
                }
                "await" if tail.first() == Some(&"gone") && !self.modern => {
                    Ok(usage("await <idle|seq|match|block>"))
                }
                "await" if tail.first() == Some(&"seq") => {
                    if let Some(r) = self.replies.pop_front() {
                        return Ok(r);
                    }
                    let stale = tail
                        .get(1)
                        .and_then(|n| n.parse::<u64>().ok())
                        .is_some_and(|n| n > self.seq);
                    if self.served < self.screens.len() && !stale {
                        return Ok(ok("OK seq\n"));
                    }
                    if let Some(n) = self.vanish_after {
                        if self.stalls >= n {
                            return Ok(err("exited"));
                        }
                        self.stalls += 1;
                    }
                    Ok(timeout())
                }
                "await" if tail.first() == Some(&"idle") && self.idle_skips_busy => {
                    if let Some(r) = self.replies.pop_front() {
                        return Ok(r);
                    }
                    while self.served + 1 < self.screens.len()
                        && worker_phase(&self.screens[self.served]) == Phase::Busy
                    {
                        self.served += 1;
                        self.seq += 1;
                    }
                    Ok(ok("OK idle\n"))
                }
                "key" => {
                    if let Some(r) = self.key_replies.pop_front() {
                        return Ok(r);
                    }
                    let guard = tail.first().and_then(|a| a.strip_prefix("if="));
                    if guard.is_some() && !self.modern {
                        return Ok(bare_err());
                    }
                    let seq = self.seq;
                    if let Some(pattern) = guard {
                        let Ok(matcher) = aterm_observe::row_matcher(pattern) else {
                            return Ok(err("badregex"));
                        };
                        if !self.last_served().iter().any(|r| matcher.matches(r)) {
                            return Ok(ok(&format!("OK skipped seq={seq}\n")));
                        }
                    }
                    Ok(ok(&format!("OK seq={seq}\n")))
                }
                // A host without the verbs answers what an older build does.
                "history" => Ok(self
                    .history
                    .clone()
                    .unwrap_or_else(|| err("unknown verb (try: help)"))),
                "offscreen" => Ok(self
                    .offscreen
                    .pop_front()
                    .unwrap_or_else(|| err("unknown verb (try: help)"))),
                _ => Ok(self.replies.pop_front().unwrap_or_else(|| ok("OK\n"))),
            }
        }
    }

    impl Ctl for Mock {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            let at = self.requests.len();
            if let Some(pause) = self.delay.remove(&at) {
                std::thread::sleep(pause);
            }
            if let Some(replies) = self.release_at.remove(&at)
                && let Some(tx) = &self.release
            {
                for r in replies {
                    let _ = tx.send(r);
                }
            }
            if let Some((i, seq)) = self.restart
                && i == at
            {
                self.seq = seq;
                self.restart = None;
            }
            let verb = args
                .iter()
                .find(|a| !a.starts_with('@'))
                .copied()
                .unwrap_or("");
            let scripted = self
                .by_index
                .remove(&at)
                .or_else(|| {
                    self.down
                        .as_ref()
                        .filter(|(from, _)| at >= *from)
                        .map(|(_, r)| r.clone())
                })
                .or_else(|| {
                    (verb == "await" && self.drop_awaits_from.is_some_and(|d| at >= d)).then(closed)
                });
            let reply = match scripted {
                Some(r) => {
                    self.requests.push(args.join(" "));
                    r
                }
                None => self.serve(args)?,
            };
            if reply.lost()
                && let Some(seq) = self.handoff_seq.take()
            {
                self.seq = seq;
            }
            Ok(reply)
        }
    }

    fn busy_screen() -> Vec<String> {
        let mut r = rows(&["⏺ Working.", "", "✻ Synthesizing… (18s)", ""]);
        r.extend(composer("  esc to interrupt"));
        r
    }
    fn idle_screen() -> Vec<String> {
        let mut r = rows(&["⏺ Done.", "", "✻ Cogitated for 4s · done 2:41 PM", ""]);
        r.extend(composer("  ? for shortcuts"));
        r
    }
    fn write_prompt() -> Vec<String> {
        let mut r = bash_one_row();
        r[4] = "   rm -rf target".to_string();
        r
    }
    fn notes_file(tag: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("aterm-supervise-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let notes = dir.join("notes.txt");
        let _ = std::fs::remove_file(&notes);
        (dir, notes)
    }
    fn read_notes(dir: &Path, notes: &Path) -> Vec<String> {
        let text = std::fs::read_to_string(notes).unwrap_or_default();
        let lines = text.lines().map(str::to_string).collect();
        let _ = std::fs::remove_file(notes);
        let _ = std::fs::remove_dir(dir);
        lines
    }
    fn auto(max_s: u64, notes: Option<PathBuf>) -> SuperviseOpts {
        SuperviseOpts {
            auto_reads: true,
            max: Duration::from_secs(max_s),
            python_allow: vec![],
            notes,
            report: false,
            dismiss_surveys: false,
            // The CLI's default: with no indicator on a screen, nothing moves.
            context_warn: 10,
            journal: None,
            mail: None,
        }
    }

    /// Modern host: the first wait is `await gone`, reads use `tail=40`, and a
    /// busy read is followed by `await seq <seq>` before the next idle wait.
    #[test]
    fn await_turn_uses_gone_then_idle_seq_on_a_modern_host() {
        let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
        let mut s = Session::new(&mut m, Some("@s-1".to_string()));
        let t = s.await_turn(Duration::from_secs(30)).expect("turn");
        assert_eq!(t.phase, Phase::Idle);
        assert!(!t.timed_out);
        assert_eq!(s.caps().gone, Some(true));
        assert_eq!(s.caps().tail, Some(true));
        assert_eq!(
            m.requests,
            [
                "@s-1 await gone esc.to.interrupt timeout 20000",
                "@s-1 text --json tail=40",
                "@s-1 await seq 101 timeout 20000",
                "@s-1 await idle 2000 timeout 20000",
                "@s-1 text --json tail=40",
            ]
        );
    }

    /// Older host: `gone` and `tail=` answer `ERR usage` ONCE each, the loop
    /// falls back to idle and the full read, and never asks again.
    #[test]
    fn await_turn_probes_once_and_falls_back_on_an_older_host() {
        let mut m = Mock::new(false, vec![busy_screen(), idle_screen()]);
        let mut s = Session::new(&mut m, None);
        let t = s.await_turn(Duration::from_secs(30)).expect("turn");
        assert_eq!(t.phase, Phase::Idle);
        assert_eq!(s.caps().gone, Some(false));
        assert_eq!(s.caps().tail, Some(false));
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "text --json",
                "await seq 101 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json",
            ]
        );
    }

    /// A spent budget with the worker still busy is a timed-out turn, and
    /// `supervise` reports it as TIMEOUT / 124.
    #[test]
    fn a_spent_budget_is_timeout_124() {
        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut s = Session::new(&mut m, None);
        let t = s.await_turn(Duration::ZERO).expect("turn");
        assert_eq!(t.phase, Phase::Busy);
        assert!(t.timed_out);

        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&SuperviseOpts {
                max: Duration::ZERO,
                ..SuperviseOpts::default()
            })
            .expect("supervise");
        assert_eq!(code, EXIT_TIMEOUT);
        assert!(
            out.starts_with("TIMEOUT\nbusy\nreason status row: spinner\n--\n"),
            "{out}"
        );
        assert!(out.contains("✻ Synthesizing"), "{out}");
    }

    /// `--auto-reads`: a read-only Bash prompt is approved with the guarded
    /// press, noted, the loop waits for the box to LEAVE (`await seq`) and
    /// continues with the settle wait, not `await gone`; the next (write)
    /// prompt is handed to the manager with the box printed.
    #[test]
    fn supervise_auto_approves_reads_guarded_and_hands_over_writes() {
        let (dir, notes) = notes_file("approve");
        let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), write_prompt()]);
        let mut s = Session::new(&mut m, Some("@s-9".to_string()));
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(
            out.starts_with("prompt\nkind bash\ncommand rm -rf target\n"),
            "{out}"
        );
        assert!(out.contains("classify not-read-only rm\n"), "{out}");
        assert!(out.contains("option 1 Yes\n"), "{out}");
        assert!(out.contains("option 4 No\n"), "{out}");
        assert!(
            out.contains("--\n Bash command\n"),
            "the box is printed: {out}"
        );
        assert!(out.ends_with(" Esc to cancel · Tab to amend\n"), "{out}");
        assert_eq!(s.caps().key_if, Some(true));
        assert_eq!(
            m.requests,
            [
                "@s-9 await gone esc.to.interrupt timeout 20000",
                "@s-9 text --json tail=40",
                "@s-9 key if=Do.you.want.to.proceed 1",
                "@s-9 await seq 101 timeout 20000",
                "@s-9 await idle 2000 timeout 20000",
                "@s-9 text --json tail=40",
                "@s-9 await seq 102 timeout 20000",
                "@s-9 await idle 2000 timeout 20000",
                "@s-9 text --json tail=40",
            ],
            "one guarded press, then the box must leave before the next look; \
             after it the settle wait, not `await gone` (the footer is not up yet)"
        );
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].ends_with("Z approved read-only: git log --oneline -5"),
            "{lines:?}"
        );
        assert!(
            lines[1].ends_with("Z handed to the manager (rm): rm -rf target"),
            "{lines:?}"
        );
        assert!(lines[0].starts_with("20"), "utc stamp: {lines:?}");
    }

    /// The host this was measured on (aterm 0.81.0) answers `key if=` with a
    /// bare `ERR`, not a usage line: that is "no such form", so the press falls
    /// back — read → confirm → `key 1` → wait for the worker to take it →
    /// re-read — and a `1` that landed in the composer after the box resolved
    /// is backspaced and NOT counted as an approval.
    #[test]
    fn a_bare_err_to_the_guard_falls_back_and_a_stray_digit_is_not_an_approval() {
        let mut stray = idle_screen();
        let c = stray.iter().position(|r| r == "❯").expect("composer");
        stray[c] = "❯ 1".to_string();
        // Reads: the turn's read (prompt), the confirm read (prompt still up),
        // the re-read (resolved, digit in the composer), then idle.
        let (dir, notes) = notes_file("stray");
        let mut m = Mock::new(
            false,
            vec![bash_one_row(), bash_one_row(), stray, idle_screen()],
        );
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n--\n"), "{out}");
        assert_eq!(s.caps().key_if, Some(false), "a bare ERR is no such form");
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "text --json",
                "key if=Do.you.want.to.proceed 1",
                "text --json",
                "key 1",
                "await seq 102 timeout 2000",
                "text --json",
                "key backspace",
                "await idle 2000 timeout 20000",
                "text --json",
            ],
            "{:?}",
            m.requests
        );
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0]
                .ends_with("nothing pressed, the box had left the screen: git log --oneline -5"),
            "no approval was made: {lines:?}"
        );
    }

    /// Older host, the box still up at the confirm read: the fallback press
    /// answers it, and the loop waits for the box to leave as on a modern host.
    #[test]
    fn the_fallback_press_approves_a_box_that_is_still_up() {
        let (dir, notes) = notes_file("fallback");
        let mut m = Mock::new(
            false,
            vec![bash_one_row(), bash_one_row(), busy_screen(), idle_screen()],
        );
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n--\n"), "{out}");
        assert_eq!(
            m.presses(),
            ["key if=Do.you.want.to.proceed 1", "key 1"],
            "{:?}",
            m.requests
        );
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].ends_with("approved read-only: git log --oneline -5"),
            "{lines:?}"
        );
    }

    /// `OK skipped seq=<n>` is an answer, not an approval: nothing was
    /// pressed, so nothing is approved or noted as such — the loop looks again.
    #[test]
    fn a_skipped_guard_is_not_an_approval() {
        let (dir, notes) = notes_file("skipped");
        let mut m = Mock::new(true, vec![bash_one_row(), idle_screen()]);
        // The box resolved between the read (seq 101) and the check (seq 105).
        m.key_replies.push_back(ok("OK skipped seq=105\n"));
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n--\n"), "{out}");
        assert_eq!(s.caps().key_if, Some(true), "a skip still proves the guard");
        assert_eq!(m.presses(), ["key if=Do.you.want.to.proceed 1"]);
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0]
                .ends_with("nothing pressed, the box had left the screen: git log --oneline -5"),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("approved")), "{lines:?}");
    }

    /// A guard that misses on the VERY screen that was parsed (a Bash box with
    /// no `Do you want to proceed?` row) is the manager's, not a loop.
    #[test]
    fn a_guard_that_matches_no_row_of_the_parsed_box_is_handed_over() {
        let mut box_without_question = bash_one_row();
        box_without_question.retain(|r| !r.contains("Do you want to proceed?"));
        assert_eq!(
            parse_prompt(&box_without_question).map(|p| p.kind),
            Some(PromptKind::Bash)
        );
        let (dir, notes) = notes_file("norow");
        let mut m = Mock::new(true, vec![box_without_question]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("prompt\nkind bash\n"), "{out}");
        assert_eq!(m.presses(), ["key if=Do.you.want.to.proceed 1"]);
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("handed to the manager (the guarded press matched no row)"),
            "{lines:?}"
        );
    }

    /// The other `ERR`s a modern host can answer the guard with are never an
    /// approval: `busy sink` is retried and then pressed; `halted` and `no such
    /// session` surface as errors with nothing approved.
    #[test]
    fn guard_errors_are_retried_or_surfaced_never_approved() {
        let (dir, notes) = notes_file("busy");
        let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), idle_screen()]);
        m.key_replies.push_back(err("busy sink"));
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n--\n"), "{out}");
        assert_eq!(
            m.presses(),
            [
                "key if=Do.you.want.to.proceed 1",
                "key if=Do.you.want.to.proceed 1"
            ],
            "retried once, then the model pressed"
        );
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("approved read-only"), "{lines:?}");

        for what in ["halted", "no such session", "badregex"] {
            let (dir, notes) = notes_file("halted");
            let mut m = Mock::new(true, vec![bash_one_row()]);
            m.key_replies.push_back(err(what));
            let mut s = Session::new(&mut m, None);
            let e = s
                .supervise(&auto(30, Some(notes.clone())))
                .expect_err("surfaces");
            assert!(e.contains(&format!("ERR {what}")), "{what}: {e}");
            assert_eq!(m.presses().len(), 1, "{what}: {:?}", m.requests);
            let lines = read_notes(&dir, &notes);
            assert!(lines.is_empty(), "{what}: nothing approved: {lines:?}");
        }

        // busy sink that never clears is an error too.
        let mut m = Mock::new(true, vec![bash_one_row()]);
        for _ in 0..=BUSY_SINK_RETRIES {
            m.key_replies.push_back(err("busy sink"));
        }
        let mut s = Session::new(&mut m, None);
        let e = s.supervise(&auto(30, None)).expect_err("surfaces");
        assert!(e.contains("ERR busy sink"), "{e}");
        assert_eq!(m.presses().len(), 1 + BUSY_SINK_RETRIES as usize);
    }

    /// Without `--auto-reads` even a read-only prompt is the manager's; a
    /// question and a non-Bash prompt are too.
    #[test]
    fn without_auto_reads_every_prompt_is_handed_over() {
        let mut m = Mock::new(true, vec![bash_one_row()]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&SuperviseOpts {
                max: Duration::from_secs(5),
                ..SuperviseOpts::default()
            })
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("prompt\nkind bash\ncommand git log --oneline -5\ndescription Show the five most recent commits\nclassify read-only\n"), "{out}");
        assert!(!m.requests.iter().any(|r| r.starts_with("key")));

        let mut q = rows(&["⏺ Keep the harness or rewrite it?", ""]);
        q.extend(composer("  ? for shortcuts"));
        let mut m = Mock::new(true, vec![q]);
        let mut s = Session::new(&mut m, None);
        let (out, _) = s.supervise(&auto(5, None)).expect("supervise");
        assert!(
            out.starts_with("question\n--\n⏺ Keep the harness or rewrite it?\n"),
            "{out}"
        );
    }

    /// The defect 4268efb74 closed, from the supervisor's side: after a press
    /// the screen is NOT re-read at once. `await gone` is level-triggered and
    /// a box shows no busy footer, so an immediate read of an unchanged screen
    /// would match the same box and press a second `1` — into the composer.
    /// The loop waits for the content to move past the press; a screen that
    /// sits unchanged is pressed ONCE and handed over.
    #[test]
    fn the_same_box_is_never_pressed_twice_on_an_unchanged_screen() {
        let (dir, notes) = notes_file("unchanged");
        let mut m = Mock::new(true, vec![bash_one_row()]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("prompt\n"), "{out}");
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                "key if=Do.you.want.to.proceed 1",
                "await seq 101 timeout 20000",
                "text --json tail=40",
            ],
            "{:?}",
            m.requests
        );
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("approved read-only"), "{lines:?}");
        assert!(
            lines[1].contains("handed to the manager (the box did not change after the press)"),
            "{lines:?}"
        );
    }

    /// The same read coming back after REAL turns (the screen moved on in
    /// between) is approved twice; the third time it is handed over rather
    /// than pressed forever.
    #[test]
    fn a_read_that_keeps_coming_back_is_approved_twice_then_handed_over() {
        let (dir, notes) = notes_file("repeat");
        let mut m = Mock::new(
            true,
            vec![
                bash_one_row(),
                busy_screen(),
                bash_one_row(),
                busy_screen(),
                bash_one_row(),
            ],
        );
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(
            out.starts_with("prompt\nkind bash\ncommand git log --oneline -5\n"),
            "{out}"
        );
        assert_eq!(m.presses().len(), 2, "{:?}", m.requests);
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(
            lines[2]
                .contains("handed to the manager (the same prompt came back after two approvals)"),
            "{lines:?}"
        );
    }

    /// An `await` that times out (exit 124) is a normal step of the loop, not
    /// an error: the read that follows decides.
    #[test]
    fn an_await_timeout_is_a_step_not_an_error() {
        let mut m = Mock::new(true, vec![idle_screen()]);
        m.replies.push_back(timeout());
        let mut s = Session::new(&mut m, None);
        let t = s.await_turn(Duration::from_secs(30)).expect("turn");
        assert_eq!(t.phase, Phase::Idle);
        assert_eq!(
            s.caps().gone,
            Some(true),
            "a timeout still proves the verb exists"
        );
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40"
            ]
        );
    }

    #[test]
    fn a_hard_ctl_error_surfaces() {
        let mut m = Mock::new(true, vec![idle_screen()]);
        m.replies.push_back(err("no such session"));
        let mut s = Session::new(&mut m, Some("@nope".to_string()));
        let err = s.await_turn(Duration::from_secs(1)).expect_err("surfaces");
        assert!(err.contains("ERR no such session"), "{err}");
    }

    /// The reply readers the guard depends on, against the measured wire.
    #[test]
    fn reply_readers_match_the_wire() {
        assert_eq!(ok("OK seq=31\n").seq(), Some(31));
        assert_eq!(ok("OK skipped seq=7\n").seq(), Some(7));
        assert!(ok("OK skipped seq=7\n").skipped());
        assert!(!ok("OK seq=7\n").skipped());
        assert_eq!(ok("OK\n").seq(), None);
        assert!(bare_err().bare_err());
        assert!(bare_err().unknown_form());
        assert!(usage("key <name>").unknown_form());
        assert!(!err("busy sink").unknown_form());
        assert!(err("busy sink").is_err("busy sink"));
        assert!(err("halted").is_err("halted"));
        assert!(!err("halted").is_err("busy sink"));
        assert_eq!(err("no such session").err_text(), "ERR no such session");
        assert!(!ok("OK\n").bare_err());
    }

    fn question_screen() -> Vec<String> {
        let mut q = rows(&["⏺ Keep the harness or rewrite it?", ""]);
        q.extend(composer("  ? for shortcuts"));
        q
    }
    fn limited_screen() -> Vec<String> {
        let mut r = rows(&[
            "⏺ Running the suite again.",
            "  ⎿  You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
            "",
            "✻ Worked for 3m 2s · done 5:02 PM",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    }
    /// `body` over an idle composer frame.
    fn framed(body: &[&str]) -> Vec<String> {
        let mut r = rows(body);
        r.extend(composer("  ? for shortcuts"));
        r
    }
    /// A Bash box for `command` under `transcript`.
    fn bash_box(transcript: &[&str], command: &str) -> Vec<String> {
        let mut r = rows(transcript);
        r.extend(rows(&[
            "",
            " Bash command",
            "",
            &format!("   {command}"),
            "   Push the branch",
            "",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and don’t ask again for: git push *",
            "   3. No",
            "",
            " Esc to cancel · Tab to amend",
        ]));
        r.extend(composer("  ? for shortcuts"));
        r
    }
    /// An Edit box for one file, changing `a` to `value`, under `transcript`.
    fn edit_box(transcript: &[&str], value: &str) -> Vec<String> {
        let mut r = rows(transcript);
        r.extend(rows(&[
            "",
            " Edit file",
            "",
            "   crates/foo/src/lib.rs",
            "",
            "   12    -    let a = 1;",
            &format!("   12    +    let a = {value};"),
            "",
            " Do you want to make this edit to lib.rs?",
            " ❯ 1. Yes",
            "   2. Yes, allow all edits during this session (shift+tab)",
            "   3. No",
            "",
            " Esc to cancel · Tab to amend",
        ]));
        r.extend(composer("  ? for shortcuts"));
        r
    }
    fn busy_over(transcript: &[&str]) -> Vec<String> {
        let mut r = rows(transcript);
        r.extend(rows(&["", "✻ Pushing… (3s)", ""]));
        r.extend(composer("  esc to interrupt"));
        r
    }
    fn watch_lines(m: &mut Mock, opts: &SuperviseOpts) -> (Vec<String>, u8) {
        let mut out: Vec<u8> = Vec::new();
        let code = Session::new(m, None).watch(opts, &mut out);
        let text = String::from_utf8(out).expect("utf-8");
        (text.lines().map(str::to_string).collect(), code)
    }
    /// The EVENT and APPROVED lines, without the seq (which counts reads).
    fn decisions(lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                let mut words: Vec<&str> = l.split(' ').collect();
                words.retain(|w| !w.starts_with("seq="));
                words.join(" ")
            })
            .collect()
    }

    /// `phase` says WHY busy is busy, and what a limit notice says.
    #[test]
    fn render_phase_names_the_busy_rule_and_the_limit() {
        let busy = Turn {
            phase: worker_phase(&busy_screen()),
            screen: Screen {
                rows: busy_screen(),
                ..Screen::default()
            },
            timed_out: false,
        };
        assert_eq!(
            render_phase(&busy, &[]),
            "busy\nreason status row: spinner\n"
        );
        let rows = limited_screen();
        let limited = Turn {
            phase: worker_phase(&rows),
            screen: Screen {
                rows,
                ..Screen::default()
            },
            timed_out: false,
        };
        assert_eq!(
            render_phase(&limited, &[]),
            "limited\nmessage You've hit your session limit · resets 7:30pm \
             (America/Los_Angeles)\nreset 7:30pm (America/Los_Angeles)\n"
        );
    }

    /// The EVENT summary is the worker's last row, never the session survey
    /// Claude Code parks under the done row (the saved wait_bg7 screen).
    #[test]
    fn an_event_summary_is_what_the_worker_said_not_the_survey() {
        let mut rows: Vec<String> = super::super::prompt::fixtures::WAIT_BG7
            .lines()
            .map(str::to_string)
            .collect();
        rows.remove(0);
        rows.pop();
        let turn = Turn {
            phase: worker_phase(&rows),
            screen: Screen {
                rows,
                seq: 7,
                ..Screen::default()
            },
            timed_out: false,
        };
        assert_eq!(
            event_line(&turn, &[]),
            "EVENT idle seq=7 bookkeeping, then site B, then compaction, then the post-merge \
             reschedule. Waiting for your call."
        );
    }

    /// `supervise` hands a limit notice to the manager exactly like idle.
    #[test]
    fn supervise_hands_a_limit_notice_over_like_idle() {
        let mut m = Mock::new(true, vec![busy_screen(), limited_screen()]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s.supervise(&auto(30, None)).expect("supervise");
        assert_eq!(code, 0);
        assert!(
            out.starts_with(
                "limited\nmessage You've hit your session limit · resets 7:30pm \
                 (America/Los_Angeles)\nreset 7:30pm (America/Los_Angeles)\n--\n"
            ),
            "{out}"
        );
        assert!(m.presses().is_empty());
    }

    /// The watch loop, scripted: busy → a read-only prompt (approved, one
    /// `APPROVED` line) → busy → a question (one `EVENT`) → the manager's turn
    /// (busy) → idle (one `EVENT`) → the idle screen sits unchanged (no second
    /// `EVENT`, however long; each wait step that runs out is checked with a
    /// read) → the session ends under the wait (`ERR exited`,
    /// `EXIT session gone`, 1). After each EVENT the loop waits for the screen
    /// to move past it, then settles before the next look.
    #[test]
    fn watch_prints_one_line_per_decision_and_keeps_watching() {
        let (dir, notes) = notes_file("watch");
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                bash_one_row(),
                busy_screen(),
                question_screen(),
                busy_screen(),
                idle_screen(),
            ],
        );
        m.vanish_after = Some(3);
        let (lines, code) = watch_lines(&mut m, &auto(30, Some(notes.clone())));
        assert_eq!(
            lines,
            [
                "APPROVED seq=102 git log --oneline -5",
                "EVENT question seq=104 ⏺ Keep the harness or rewrite it?",
                "EVENT idle seq=106 ⏺ Done.",
                "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                "await seq 101 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "key if=Do.you.want.to.proceed 1",
                "await seq 102 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "await seq 103 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                // EVENT question: wait for the screen to move past it.
                "await seq 104 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "await seq 105 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                // EVENT idle: the screen never moves again. Each step that
                // runs out is checked with a read — its seq is not below 106,
                // so the count did not start over — and nothing is reported.
                "await seq 106 timeout 20000",
                "text --json tail=40",
                "await seq 106 timeout 20000",
                "text --json tail=40",
                "await seq 106 timeout 20000",
                "text --json tail=40",
                "await seq 106 timeout 20000",
            ],
            "{:?}",
            m.requests
        );
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].ends_with("approved read-only: git log --oneline -5"),
            "{lines:?}"
        );
    }

    /// An `offscreen … screen=1` reply as `aterm ctl` prints it: the rows on
    /// stdout, the header on stderr.
    fn offscreen_reply(first: u64, archived: &[&str], screen: &[String]) -> CtlReply {
        let n = u64::try_from(archived.len()).expect("fits");
        let mut stdout: String = archived.iter().map(|r| format!("{r}\n")).collect();
        stdout.extend(screen.iter().map(|r| format!("{r}\n")));
        CtlReply {
            code: 0,
            stdout,
            stderr: format!(
                "aterm-ctl: OK {} first={first} last={} lost=0 breaks=0 back=0 epoch=1 \
                 origin=5 alt=1 seq=1 screen_rows={}\n",
                archived.len() + screen.len(),
                (first + n).saturating_sub(1),
                screen.len()
            ),
        }
    }

    /// `watch --report`: an idle, question or limited point's EVENT line
    /// carries `complete=<0|1> rows=<n>` of the report — read at the point,
    /// `history` then one `offscreen` — before the summary; an approval and
    /// a prompt's EVENT are as without the flag, and nothing else changes.
    #[test]
    fn watch_report_counts_the_report_into_the_events_that_end_on_words() {
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                bash_one_row(),
                busy_screen(),
                question_screen(),
                busy_screen(),
                idle_screen(),
            ],
        );
        m.vanish_after = Some(1);
        m.history = Some(ok(
            "turn 7 submitted=1 status=settled started_ms=1 dur_ms=2 seq=3 \
             hash=0000000000000000 arch=5:40 text=Pick%20one\n",
        ));
        // At the question: the turn's row scrolled off (archived 41..=42).
        m.offscreen.push_back(offscreen_reply(
            41,
            &["❯ Pick one", "⏺ Looking at both."],
            &question_screen(),
        ));
        // At the idle point: the turn's row is nowhere to be read.
        m.offscreen
            .push_back(offscreen_reply(41, &[], &idle_screen()));
        let opts = SuperviseOpts {
            report: true,
            ..auto(30, None)
        };
        let (lines, code) = watch_lines(&mut m, &opts);
        assert_eq!(
            lines,
            [
                "APPROVED seq=102 git log --oneline -5",
                "EVENT question seq=104 complete=1 rows=3 ⏺ Keep the harness or rewrite it?",
                "EVENT idle seq=106 complete=0 rows=3 ⏺ Done.",
                "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        let reads: Vec<&String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("history") || r.starts_with("offscreen"))
            .collect();
        assert_eq!(
            reads,
            [
                "history 8",
                "offscreen since=5:40 max=8000 screen=1",
                "history 8",
                "offscreen since=5:40 max=8000 screen=1",
            ]
        );
        // A prompt's point counts no report, and busy is never a point.
        assert!(!reported(&Phase::Prompt) && !reported(&Phase::Busy));
        let mut m = Mock::new(true, vec![write_prompt()]);
        m.vanish_after = Some(1);
        let (lines, _) = watch_lines(&mut m, &opts);
        assert_eq!(
            lines[0],
            "EVENT prompt seq=101 kind=bash classify=not-read-only:rm command=rm -rf target"
        );
        assert!(
            !m.requests
                .iter()
                .any(|r| r.starts_with("history") || r.starts_with("offscreen")),
            "{:?}",
            m.requests
        );
    }

    /// `supervise` prints the rows themselves: `report` reads nothing there.
    #[test]
    fn supervise_reads_no_report() {
        let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
        let opts = SuperviseOpts {
            report: true,
            ..auto(30, None)
        };
        let (out, code) = Session::new(&mut m, None)
            .supervise(&opts)
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n--\n"), "{out}");
        assert!(
            !m.requests
                .iter()
                .any(|r| r.starts_with("history") || r.starts_with("offscreen")),
            "{:?}",
            m.requests
        );
    }

    /// A report that fails for any reason but a lost connection does not end
    /// `watch`: the point is still reported, `complete=0 rows=0`, and the
    /// failure goes to the notes.
    #[test]
    fn a_report_that_fails_still_reports_the_point() {
        let (dir, notes) = notes_file("report-fails");
        let mut m = Mock::new(true, vec![idle_screen()]);
        m.vanish_after = Some(1);
        m.offscreen.push_back(CtlReply {
            code: 0,
            stdout: "one row\n".to_string(),
            stderr: "aterm-ctl: OK 3 first=1 last=1 lost=0 breaks=0 back=0 origin=5 alt=1\n"
                .to_string(),
        });
        let opts = SuperviseOpts {
            report: true,
            ..auto(30, Some(notes.clone()))
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        assert_eq!(lines[0], "EVENT idle seq=101 complete=0 rows=0 ⏺ Done.");
        let noted = read_notes(&dir, &notes);
        assert!(
            noted.len() == 1 && noted[0].contains("report failed: offscreen: the header counts 3"),
            "{noted:?}"
        );
    }

    /// `watch --report` against a host without `offscreen`: the report is the
    /// screen alone (a full `text --json`), never complete.
    #[test]
    fn watch_report_on_an_old_host_reads_the_screen_alone() {
        let mut m = Mock::new(true, vec![idle_screen()]);
        m.vanish_after = Some(1);
        let opts = SuperviseOpts {
            report: true,
            ..auto(30, None)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        assert_eq!(lines[0], "EVENT idle seq=101 complete=0 rows=3 ⏺ Done.");
        let at = m
            .requests
            .iter()
            .position(|r| r == "history 8")
            .expect("history read");
        assert_eq!(
            m.requests[at..at + 3],
            [
                "history 8",
                "offscreen tail=8000 max=8000 screen=1",
                "text --json"
            ]
        );
    }

    /// The server as it is: `await idle 2000` waits straight through a short
    /// busy spell, so no read sees the worker busy between two points. A
    /// screen that moved with nothing the point is made of changed (a footer
    /// tick) prints no second EVENT; the same last row after a real turn — the
    /// manager's row and a new done row above it — is a new point.
    #[test]
    fn a_point_is_new_when_its_transcript_moved_whatever_the_loop_saw() {
        let ticked = {
            let mut r = question_screen();
            let last = r.len() - 1;
            r[last] = "  ? for shortcuts                      tick 2".to_string();
            r
        };
        let mut m = Mock::new(true, vec![question_screen(), ticked]);
        m.idle_skips_busy = true;
        m.vanish_after = Some(1);
        let (lines, code) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "EVENT question seq=101 ⏺ Keep the harness or rewrite it?",
                "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);

        let first = framed(&[
            "❯ keep going",
            "",
            "⏺ Done.",
            "",
            "✻ Cogitated for 4s · done 10:40 AM",
            "",
        ]);
        let again = framed(&[
            "❯ keep going",
            "",
            "⏺ Done.",
            "",
            "✻ Cogitated for 4s · done 10:40 AM",
            "",
            "❯ and the docs",
            "",
            "⏺ Done.",
            "",
            "✻ Cogitated for 3s · done 10:40 AM",
            "",
        ]);
        let mut m = Mock::new(true, vec![first, busy_screen(), again]);
        m.idle_skips_busy = true;
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "EVENT idle seq=101 ⏺ Done.",
                "EVENT idle seq=103 ⏺ Done.",
                "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(
            m.requests.iter().filter(|r| r.starts_with("text")).count(),
            2,
            "two reads, neither of them busy: {:?}",
            m.requests
        );
    }

    /// The reviewers' live cases, each a new point after a busy spell no read
    /// saw: the same `git push` asked again after it failed; a second Edit box
    /// for the same file; the same hunk's file with a new hunk and nothing new
    /// above it; a plain shell back at its `$` after another command; a retry
    /// that hit the limit wall again.
    #[test]
    fn a_new_point_that_looks_like_the_last_one_is_still_reported() {
        let pushed = [
            "⏺ Pushing the fix.",
            "",
            "⏺ Bash(git push origin main)",
            "  ⎿  error: failed to push some refs (rejected: fetch first)",
            "",
            "⏺ The remote moved; retrying the push.",
        ];
        let updated = [
            "⏺ Update(crates/foo/src/lib.rs)",
            "  ⎿  Updated crates/foo/src/lib.rs with 1 addition and 1 removal",
        ];
        let mut second_site = updated.to_vec();
        second_site.extend(["", "⏺ And the second site in the same file."]);
        let retried = framed(&[
            "⏺ Running the suite again.",
            "  ⎿  You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
            "",
            "✻ Worked for 3m 2s · done 5:02 PM",
            "",
            "❯ continue",
            "  ⎿  You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
            "",
            "✻ Churned for 0s · done 5:02 PM",
            "",
        ]);
        // (A shell has no busy footer, so the first read on a modern host is
        // taken before the output settled; an older host's first wait is the
        // idle one, which keeps this script's first screen its first point.)
        for (case, modern, script, want) in [
            (
                "a retried push",
                true,
                vec![
                    bash_box(&["⏺ Pushing the fix."], "git push origin main"),
                    busy_over(&pushed[..4]),
                    bash_box(&pushed, "git push origin main"),
                ],
                "EVENT prompt kind=bash classify=not-read-only:git push command=git push origin main",
            ),
            (
                "a second edit to one file",
                true,
                vec![
                    edit_box(&["⏺ Fixing the constant."], "2"),
                    busy_over(&updated),
                    edit_box(&second_site, "3"),
                ],
                "EVENT prompt kind=edit classify=- command=crates/foo/src/lib.rs",
            ),
            (
                "a new hunk, nothing new above it",
                true,
                vec![
                    edit_box(&["⏺ Fixing the constant."], "2"),
                    busy_screen(),
                    edit_box(&["⏺ Fixing the constant."], "3"),
                ],
                "EVENT prompt kind=edit classify=- command=crates/foo/src/lib.rs",
            ),
            (
                "a shell back at its prompt",
                false,
                vec![
                    rows(&["$ make", "ok", "$"]),
                    rows(&["$ make", "ok", "$ ls", "a b c", "$"]),
                ],
                "EVENT idle $",
            ),
            (
                "a retry into the wall",
                true,
                vec![limited_screen(), busy_screen(), retried],
                "EVENT limited message=You've hit your session limit · resets 7:30pm \
                 (America/Los_Angeles) reset=7:30pm (America/Los_Angeles)",
            ),
        ] {
            let mut m = Mock::new(modern, script);
            m.idle_skips_busy = true;
            m.vanish_after = Some(0);
            let (lines, code) = watch_lines(
                &mut m,
                &SuperviseOpts {
                    max: Duration::from_secs(30),
                    ..SuperviseOpts::default()
                },
            );
            let got = decisions(&lines);
            assert_eq!(&got[..2], [want, want], "{case}: {lines:?}");
            assert!(got[2].starts_with("EXIT session gone"), "{case}: {lines:?}");
            assert_eq!(got.len(), 3, "{case}: {lines:?}");
            assert_eq!(code, 1, "{case}");
            assert!(
                !m.requests.iter().any(|r| r.starts_with("key")),
                "{case}: nothing pressed: {:?}",
                m.requests
            );
        }
    }

    /// A read-only prompt handed over (it came back after two approvals) is
    /// not pressed when the screen moves under it without the worker running;
    /// the other review points print their own summaries.
    #[test]
    fn watch_summarises_each_kind_of_review_point() {
        let mut m = Mock::new(
            true,
            vec![
                write_prompt(),
                busy_screen(),
                limited_screen(),
                busy_screen(),
                workflow_box(),
            ],
        );
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "EVENT prompt seq=101 kind=bash classify=not-read-only:rm command=rm -rf target",
                "EVENT limited seq=103 message=You've hit your session limit · resets 7:30pm \
                 (America/Los_Angeles) reset=7:30pm (America/Los_Angeles)",
                "EVENT prompt seq=105 kind=workflow classify=- command=Fan out the 12 benchmark \
                 families to 4 agents and collect the standings table.",
                "EXIT session gone (await seq 105 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert!(m.presses().is_empty(), "{:?}", m.requests);

        // Two approvals, then the same read is the manager's — and stays the
        // manager's while the screen moves under it with the worker idle.
        let mut m = Mock::new(
            true,
            vec![
                bash_one_row(),
                busy_screen(),
                bash_one_row(),
                busy_screen(),
                bash_one_row(),
                bash_one_row(),
            ],
        );
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "APPROVED seq=101 git log --oneline -5",
                "APPROVED seq=103 git log --oneline -5",
                "EVENT prompt seq=105 kind=bash classify=read-only command=git log --oneline -5",
                "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(m.presses().len(), 2, "{:?}", m.requests);
    }

    /// A worker without Claude Code's composer — a build — whose output never
    /// held still for the idle window is still running: no review point until
    /// it pauses. The one read taken while it streamed is not handed over.
    #[test]
    fn a_frameless_screen_still_changing_is_busy_until_it_settles() {
        let mut m = Mock::new(
            true,
            vec![
                rows(&["$ make", "compiling a"]),
                rows(&["$ make", "compiling a", "compiling b", "done", "$"]),
            ],
        );
        // `await gone` latches at once (no busy footer on a shell); the first
        // `await idle` runs out its step: the output never paused.
        m.replies.push_back(ok("OK gone 100\n"));
        m.replies.push_back(timeout());
        let mut s = Session::new(&mut m, None);
        let (out, code) = s.supervise(&auto(30, None)).expect("supervise");
        assert_eq!(code, 0);
        assert_eq!(out, "idle\n--\n$ make\ncompiling a\ncompiling b\ndone\n$\n");
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "await seq 102 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
            ]
        );
        // What `phase` would print for such a turn if the budget ran out.
        let streaming = Turn {
            phase: Phase::Busy,
            screen: Screen {
                rows: rows(&["$ make", "compiling a"]),
                ..Screen::default()
            },
            timed_out: true,
        };
        assert_eq!(
            render_phase(&streaming, &[]),
            "busy\nreason whole screen, no composer frame: output still changing\n"
        );
    }

    /// The budget bounds every action: a turn read at or after the deadline
    /// is the TIMEOUT — `watch` prints no EVENT before it, and `supervise`
    /// presses nothing, whatever the phase — with the last read's lines; and
    /// so is a point decided after it, a pressed box that never left.
    #[test]
    fn nothing_happens_after_the_budget() {
        let spent = SuperviseOpts {
            max: Duration::ZERO,
            ..SuperviseOpts::default()
        };
        let mut m = Mock::new(true, vec![busy_screen()]);
        let (lines, code) = watch_lines(&mut m, &spent);
        assert_eq!((lines, code), (vec!["TIMEOUT".to_string()], EXIT_TIMEOUT));

        let mut m = Mock::new(true, vec![idle_screen()]);
        let (lines, code) = watch_lines(&mut m, &spent);
        assert_eq!((lines, code), (vec!["TIMEOUT".to_string()], EXIT_TIMEOUT));

        let mut m = Mock::new(true, vec![bash_one_row()]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s.supervise(&auto(0, None)).expect("supervise");
        assert_eq!(code, EXIT_TIMEOUT);
        assert!(out.starts_with("TIMEOUT\nprompt\nkind bash\n"), "{out}");
        assert!(m.presses().is_empty(), "{:?}", m.requests);

        let mut m = Mock::new(true, vec![idle_screen()]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s.supervise(&spent).expect("supervise");
        assert_eq!(code, EXIT_TIMEOUT);
        assert!(out.starts_with("TIMEOUT\nidle\n--\n"), "{out}");

        // A press in the budget, then the wait for its box to leave runs out
        // with it: the approval stands, and the box that did not move is the
        // TIMEOUT, not an EVENT printed after the deadline.
        let mut m = Mock::new(true, vec![bash_one_row()]);
        m.delay.insert(3, Duration::from_millis(200));
        let opts = SuperviseOpts {
            max: Duration::from_millis(100),
            ..auto(0, None)
        };
        let (lines, code) = watch_lines(&mut m, &opts);
        assert_eq!(
            (lines, code),
            (
                vec![
                    "APPROVED seq=101 git log --oneline -5".to_string(),
                    "TIMEOUT".to_string()
                ],
                EXIT_TIMEOUT
            )
        );
        assert_eq!(m.requests.len(), 4, "nothing read after: {:?}", m.requests);
    }

    #[test]
    fn watch_lines_are_cut_at_160_characters() {
        let long = "x".repeat(400);
        assert_eq!(clip(&long).chars().count(), LINE_CHARS);
        assert_eq!(clip("a\tb\nc"), "a b c");
        assert_eq!(exit_reason("ERR halted\n"), "ERR halted");
        assert_eq!(
            exit_reason("text --json tail=40 failed: aterm-ctl: ERR no such session"),
            "session gone (text --json tail=40 failed: aterm-ctl: ERR no such session)"
        );
        assert_eq!(
            exit_reason("await seq 633 failed: aterm-ctl: ERR exited"),
            "session gone (await seq 633 failed: aterm-ctl: ERR exited)"
        );
    }

    // ---- the session survey ----------------------------------------------

    /// Claude Code's session survey, as it parks above the composer.
    const SURVEY_ROWS: [&str; 2] = [
        "● How is Claude doing this session? (optional)",
        "  1: Bad    2: Fine   3: Good   0: Dismiss",
    ];

    /// `r` with the survey parked right above its composer's top rule.
    fn with_survey(mut r: Vec<String>) -> Vec<String> {
        let top = r
            .iter()
            .position(|row| row.starts_with('─'))
            .expect("the top rule");
        for (k, row) in SURVEY_ROWS.iter().enumerate() {
            r.insert(top + k, (*row).to_string());
        }
        r
    }

    /// `phase` ends with `survey 0` when the survey is open — the key that
    /// dismisses it, under whatever it printed before — and is `render_phase`
    /// alone otherwise, a survey quoted in the transcript included.
    #[test]
    fn phase_names_the_key_that_dismisses_an_open_survey() {
        let turn = |r: Vec<String>| Turn {
            phase: worker_phase(&r),
            screen: Screen {
                rows: r,
                ..Screen::default()
            },
            timed_out: false,
        };
        let open = turn(with_survey(idle_screen()));
        assert_eq!(render_phase_and_survey(&open, &[]), "idle\nsurvey 0\n");
        let busy = turn(with_survey(busy_screen()));
        assert_eq!(
            render_phase_and_survey(&busy, &[]),
            "busy\nreason status row: spinner\nsurvey 0\n"
        );
        let quoted = turn(framed(&[
            "⏺ It asked:",
            "  ● How is Claude doing this session? (optional)",
            "    1: Bad    2: Fine   3: Good   0: Dismiss",
            "",
        ]));
        assert_eq!(render_phase_and_survey(&quoted, &[]), "idle\n");
        let plain = turn(idle_screen());
        assert_eq!(
            render_phase_and_survey(&plain, &[]),
            render_phase(&plain, &[])
        );
    }

    /// A survey that appears on an idle screen is said ONCE, ahead of the
    /// point: `EVENT survey seq=<n> dismiss with: aterm ctl … key
    /// 'if=^●.How.is.Claude.doing' 0`. It stays open through the next turn
    /// (busy under it) and the idle point after: nothing more is said about
    /// it, and nothing is pressed.
    #[test]
    fn watch_says_a_survey_once_when_it_appears() {
        let mut m = Mock::new(
            true,
            vec![
                with_survey(idle_screen()),
                with_survey(busy_screen()),
                with_survey(said_idle("⏺ Pushed.")),
            ],
        );
        m.vanish_after = Some(1);
        let (lines, code) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "EVENT survey seq=101 dismiss with: aterm ctl key 'if=^●.How.is.Claude.doing' 0",
                "EVENT idle seq=101 ⏺ Done.",
                "EVENT idle seq=103 ⏺ Pushed.",
                "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        assert!(m.presses().is_empty(), "{:?}", m.requests);
        assert_eq!(
            survey_event_line(7, Some("@s-1")),
            "EVENT survey seq=7 dismiss with: aterm ctl @s-1 key 'if=^●.How.is.Claude.doing' 0"
        );
    }

    /// `--dismiss-surveys`: the survey that appears gets ONE guarded `0` —
    /// `key if=^●.How.is.Claude.doing 0`, and no other key — and the loop
    /// looks again from a fresh read: the survey gone there, the `0` is said
    /// as `DISMISSED survey seq=<n>` (the press's seq) and noted, and the
    /// point under it reported. `supervise` says the line on stderr, never in
    /// its result.
    #[test]
    fn dismiss_surveys_presses_one_guarded_0() {
        let (dir, notes) = notes_file("survey");
        let mut m = Mock::new(true, vec![with_survey(idle_screen()), idle_screen()]);
        m.vanish_after = Some(1);
        let opts = SuperviseOpts {
            dismiss_surveys: true,
            ..auto(30, Some(notes.clone()))
        };
        let (lines, code) = watch_lines(&mut m, &opts);
        assert_eq!(
            lines,
            [
                "DISMISSED survey seq=101",
                "EVENT idle seq=102 ⏺ Done.",
                "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                "key if=^●.How.is.Claude.doing 0",
                // Looked at again from a fresh read: the settle wait first.
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "await seq 102 timeout 20000",
                "text --json tail=40",
                "await seq 102 timeout 20000",
            ],
            "{:?}",
            m.requests
        );
        let noted = read_notes(&dir, &notes);
        assert_eq!(noted.len(), 1, "{noted:?}");
        assert!(
            noted[0]
                .ends_with("Z dismissed the session survey (pressed 0; a rating is the human's)"),
            "{noted:?}"
        );

        let mut m = Mock::new(true, vec![with_survey(idle_screen()), idle_screen()]);
        let opts = SuperviseOpts {
            dismiss_surveys: true,
            ..auto(30, None)
        };
        let mut log: Vec<u8> = Vec::new();
        let (out, code) = Session::new(&mut m, Some("@s-1".to_string()))
            .supervise_to(&opts, &mut log)
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n--\n⏺ Done.\n"), "{out}");
        assert!(!out.contains("DISMISSED"), "{out}");
        let log = String::from_utf8(log).expect("utf-8");
        assert_eq!(log, "DISMISSED survey seq=101\n");
        assert_eq!(m.presses(), ["@s-1 key if=^●.How.is.Claude.doing 0"]);
    }

    /// A guarded `0` that finds no survey row — it left between the read and
    /// the check, `OK skipped seq=<n>` — pressed nothing: no DISMISSED line,
    /// no note, and the loop looks again (where the survey is gone).
    #[test]
    fn a_skipped_survey_press_says_nothing() {
        let (dir, notes) = notes_file("survey-skipped");
        let mut m = Mock::new(true, vec![with_survey(idle_screen()), idle_screen()]);
        m.key_replies.push_back(ok("OK skipped seq=105\n"));
        m.vanish_after = Some(0);
        let opts = SuperviseOpts {
            dismiss_surveys: true,
            ..auto(30, Some(notes.clone()))
        };
        let (lines, code) = watch_lines(&mut m, &opts);
        assert_eq!(
            lines,
            [
                "EVENT idle seq=102 ⏺ Done.",
                "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        assert_eq!(m.presses(), ["key if=^●.How.is.Claude.doing 0"]);
        let noted = read_notes(&dir, &notes);
        assert!(noted.is_empty(), "{noted:?}");
    }

    /// A survey on a screen with a box waits for the box: the box is handled
    /// first — a read approved, anything else reported — and while it is up
    /// no `0` is pressed and the survey is not said. Once the box has gone
    /// the survey is dismissed like any other, and said so once it has left.
    #[test]
    fn a_survey_waits_while_a_box_is_up() {
        let mut m = Mock::new(
            true,
            vec![
                with_survey(bash_one_row()),
                with_survey(busy_screen()),
                with_survey(idle_screen()),
                idle_screen(),
            ],
        );
        m.vanish_after = Some(1);
        let opts = SuperviseOpts {
            dismiss_surveys: true,
            ..auto(30, None)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        assert_eq!(
            lines,
            [
                "APPROVED seq=101 git log --oneline -5",
                "DISMISSED survey seq=103",
                "EVENT idle seq=104 ⏺ Done.",
                "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(
            m.presses(),
            [
                "key if=Do.you.want.to.proceed 1",
                "key if=^●.How.is.Claude.doing 0"
            ],
            "{:?}",
            m.requests
        );

        // A box handed over is reported, and the survey under it is neither
        // pressed nor said — with --dismiss-surveys or without.
        for dismiss_surveys in [true, false] {
            let mut m = Mock::new(true, vec![with_survey(write_prompt())]);
            m.vanish_after = Some(0);
            let opts = SuperviseOpts {
                dismiss_surveys,
                ..auto(30, None)
            };
            let (lines, _) = watch_lines(&mut m, &opts);
            assert_eq!(
                lines,
                [
                    "EVENT prompt seq=101 kind=bash classify=not-read-only:rm command=rm -rf target",
                    "EXIT session gone (await seq 101 failed: aterm-ctl: ERR exited)",
                ],
                "dismiss_surveys={dismiss_surveys}"
            );
            assert!(m.presses().is_empty(), "{:?}", m.requests);
        }
    }

    /// A host without `key if=` never gets a `0`, guarded or not: the one
    /// probe answers a bare `ERR`, and the survey is said instead.
    #[test]
    fn a_survey_on_a_host_without_the_guard_is_said_not_pressed() {
        let mut m = Mock::new(false, vec![with_survey(idle_screen())]);
        m.vanish_after = Some(0);
        let opts = SuperviseOpts {
            dismiss_surveys: true,
            ..auto(30, None)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        assert_eq!(
            lines,
            [
                "EVENT survey seq=101 dismiss with: aterm ctl key 'if=^●.How.is.Claude.doing' 0",
                "EVENT idle seq=101 ⏺ Done.",
                "EXIT session gone (await seq 101 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(m.presses(), ["key if=^●.How.is.Claude.doing 0"]);
    }

    /// `--dismiss-surveys` on the defaults of the other survey tests.
    fn dismissing(notes: Option<PathBuf>) -> SuperviseOpts {
        SuperviseOpts {
            dismiss_surveys: true,
            ..auto(30, notes)
        }
    }

    /// The guard the survey's `0` is pressed under is tested against EVERY
    /// row on the screen (the server's `row_matcher`), so it matches the open
    /// survey's question row and no copy of it: a tool's output under `⎿`,
    /// the worker quoting it, the guard's own words in an EVENT line or in
    /// `--help`. Such a copy on the screen when the survey itself has gone
    /// lets no `0` through to the composer. (Round-8 review, finding 1: the
    /// bare `How.is.Claude.doing` matched all three.)
    #[test]
    fn the_survey_guard_matches_the_open_survey_and_no_copy_of_it() {
        let guard = aterm_observe::row_matcher(SURVEY_ROW).expect("the guard compiles");
        let open = with_survey(idle_screen());
        assert!(survey_open(&open));
        assert_eq!(
            open.iter().filter(|r| guard.matches(r)).collect::<Vec<_>>(),
            [SURVEY_ROWS[0]]
        );
        let event = survey_event_line(7, Some("@s-1"));
        let help = format!("                       aterm ctl @sid key 'if={SURVEY_ROW}' 0");
        for copy in [
            framed(&[
                "⏺ Bash(cat survey-open.txt)",
                "  ⎿  ● How is Claude doing this session? (optional)",
                "       1: Bad    2: Fine   3: Good   0: Dismiss",
                "",
            ]),
            framed(&[
                "⏺ Claude Code parks this above the composer:",
                "  ● How is Claude doing this session? (optional)",
                "    1: Bad    2: Fine   3: Good   0: Dismiss",
                "",
            ]),
            framed(&[&event, &help, &format!("  ⎿  {event}"), ""]),
        ] {
            assert!(!survey_open(&copy), "{copy:?}");
            assert!(!copy.iter().any(|r| guard.matches(r)), "{copy:?}");
        }
        // And the mock judges it as the server does: the copy skips.
        let mut m = Mock::new(
            true,
            vec![framed(&[
                "⏺ Bash(cat survey-open.txt)",
                "  ⎿  ● How is Claude doing this session? (optional)",
                "",
            ])],
        );
        let r = Session::new(&mut m, None)
            .dismiss_survey(101)
            .expect("answered");
        assert_eq!(r, Dismiss::Skipped);
        assert_eq!(m.requests, ["key if=^●.How.is.Claude.doing 0"]);
    }

    /// A `0` pressed on a survey that is still open at the next look did not
    /// dismiss it: no DISMISSED line — the survey is handed over with the
    /// `EVENT survey` line and a note, and never pressed again while it stays
    /// open. (Round-8 review, finding 2: `DISMISSED` came at the press.)
    #[test]
    fn a_survey_still_open_after_its_0_is_handed_over_not_dismissed() {
        let (dir, notes) = notes_file("survey-stays");
        let mut m = Mock::new(true, vec![with_survey(idle_screen())]);
        m.vanish_after = Some(1);
        let (lines, _) = watch_lines(&mut m, &dismissing(Some(notes.clone())));
        assert_eq!(
            lines,
            [
                "EVENT survey seq=102 dismiss with: aterm ctl key 'if=^●.How.is.Claude.doing' 0",
                "EVENT idle seq=102 ⏺ Done.",
                "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(m.presses(), ["key if=^●.How.is.Claude.doing 0"]);
        let noted = read_notes(&dir, &notes);
        assert_eq!(noted.len(), 1, "{noted:?}");
        assert!(
            noted[0].ends_with(
                "Z handed the session survey to the manager (still open after its guarded 0)"
            ),
            "{noted:?}"
        );
    }

    /// A `0` that landed in the composer — the survey had left between the
    /// check and the keystroke reaching it — is backspaced and noted, as the
    /// fallback press's stray `1` is, and nothing is said dismissed.
    /// (Round-8 review, finding 2: it was never backspaced.)
    #[test]
    fn a_0_that_landed_in_the_composer_is_backspaced_not_dismissed() {
        let (dir, notes) = notes_file("survey-stray");
        let mut zero = idle_screen();
        let c = zero.iter().position(|r| r == "❯").expect("composer");
        zero[c] = "❯ 0".to_string();
        let mut m = Mock::new(true, vec![with_survey(idle_screen()), zero, idle_screen()]);
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &dismissing(Some(notes.clone())));
        assert_eq!(
            lines,
            [
                "EVENT idle seq=103 ⏺ Done.",
                "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(
            m.presses(),
            ["key if=^●.How.is.Claude.doing 0", "key backspace"]
        );
        let noted = read_notes(&dir, &notes);
        assert_eq!(noted.len(), 1, "{noted:?}");
        assert!(
            noted[0].ends_with(
                "Z backspaced a 0 left in the composer by the press for the session survey \
                 (it had left first; nothing was dismissed)"
            ),
            "{noted:?}"
        );

        // The survey still open over the stray `0`: backspaced all the same,
        // and the survey handed over — not pressed again.
        let mut zero = with_survey(idle_screen());
        let c = zero.iter().position(|r| r == "❯").expect("composer");
        zero[c] = "❯ 0".to_string();
        let mut m = Mock::new(
            true,
            vec![with_survey(idle_screen()), zero, with_survey(idle_screen())],
        );
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &dismissing(None));
        assert_eq!(
            lines,
            [
                "EVENT survey seq=102 dismiss with: aterm ctl key 'if=^●.How.is.Claude.doing' 0",
                "EVENT idle seq=103 ⏺ Done.",
                "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(
            m.presses(),
            ["key if=^●.How.is.Claude.doing 0", "key backspace"]
        );
    }

    /// A survey that leaves during a busy spell and comes back has appeared
    /// again: said again — or, `--dismiss-surveys`, pressed again — though
    /// no look saw it gone, only a busy read on the way to the turn.
    /// (Round-8 review, finding 4: the second was missed.)
    #[test]
    fn a_survey_back_after_a_busy_spell_is_a_new_appearance() {
        let mut m = Mock::new(
            true,
            vec![
                with_survey(idle_screen()),
                busy_screen(),
                with_survey(said_idle("⏺ Pushed.")),
            ],
        );
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                survey_event_line(101, None),
                "EVENT idle seq=101 ⏺ Done.".to_string(),
                survey_event_line(103, None),
                "EVENT idle seq=103 ⏺ Pushed.".to_string(),
                "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)".to_string(),
            ]
        );
        assert!(m.presses().is_empty(), "{:?}", m.requests);

        let mut m = Mock::new(
            true,
            vec![
                with_survey(idle_screen()),
                busy_screen(),
                with_survey(said_idle("⏺ Pushed.")),
                said_idle("⏺ Pushed."),
            ],
        );
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &dismissing(None));
        assert_eq!(
            lines,
            [
                "DISMISSED survey seq=101",
                "DISMISSED survey seq=103",
                "EVENT idle seq=104 ⏺ Pushed.",
                "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(
            m.presses(),
            [
                "key if=^●.How.is.Claude.doing 0",
                "key if=^●.How.is.Claude.doing 0"
            ]
        );
    }

    /// A guarded `0` that was skipped, with the survey still open at the next
    /// look (the guard matched no row of it), is handed over with the `EVENT
    /// survey` line — never left unsaid, and never pressed again while it
    /// stays. (Round-8 review, finding 5: it was marked handled.)
    #[test]
    fn a_skipped_0_on_a_survey_still_open_is_handed_over() {
        let (dir, notes) = notes_file("survey-skip-stays");
        let mut m = Mock::new(true, vec![with_survey(idle_screen())]);
        m.key_replies.push_back(ok("OK skipped seq=101\n"));
        m.vanish_after = Some(1);
        let (lines, _) = watch_lines(&mut m, &dismissing(Some(notes.clone())));
        assert_eq!(
            lines,
            [
                "EVENT survey seq=102 dismiss with: aterm ctl key 'if=^●.How.is.Claude.doing' 0",
                "EVENT idle seq=102 ⏺ Done.",
                "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(m.presses(), ["key if=^●.How.is.Claude.doing 0"]);
        let noted = read_notes(&dir, &notes);
        assert_eq!(noted.len(), 1, "{noted:?}");
        assert!(
            noted[0].ends_with(
                "Z handed the session survey to the manager (its guarded 0 matched no row, and \
                 it is still open)"
            ),
            "{noted:?}"
        );
    }

    /// A host without `key if=` never gets an unguarded `1` while the session
    /// survey is open on the confirming read: had the box resolved first, the
    /// `1` would be the rating `Bad`, and no digit would be left in the
    /// composer to find. The box is handed over (noted), whatever the
    /// classifier says of it. (Round-8 review, finding 6.)
    #[test]
    fn the_fallback_press_withholds_its_1_while_the_survey_is_open() {
        let (dir, notes) = notes_file("survey-fallback");
        let mut m = Mock::new(
            false,
            vec![
                with_survey(bash_one_row()),
                with_survey(bash_one_row()),
                with_survey(idle_screen()),
            ],
        );
        let (out, code) = Session::new(&mut m, None)
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(
            out.starts_with("prompt\nkind bash\ncommand git log --oneline -5\n"),
            "{out}"
        );
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "text --json",
                "key if=Do.you.want.to.proceed 1",
                "text --json",
            ],
            "{:?}",
            m.requests
        );
        let noted = read_notes(&dir, &notes);
        assert_eq!(noted.len(), 1, "{noted:?}");
        assert!(
            noted[0].ends_with(
                "Z handed to the manager (the session survey is open and this host has no \
                 guarded press: an unguarded 1 could rate the session): git log --oneline -5"
            ),
            "{noted:?}"
        );

        // With no survey the fallback presses as before.
        let mut m = Mock::new(
            false,
            vec![bash_one_row(), bash_one_row(), busy_screen(), idle_screen()],
        );
        let (_, code) = Session::new(&mut m, None)
            .supervise(&auto(30, None))
            .expect("supervise");
        assert_eq!(code, 0);
        assert_eq!(m.presses(), ["key if=Do.you.want.to.proceed 1", "key 1"]);
    }

    /// A draft can fill more than one row of the composer, and a `0` pressed
    /// then lands in it: text on a row under the caret row is typed — its
    /// first row empty or not — and so is the caret row's text when the
    /// cursor sits at column 2 of ANOTHER row (after a line break). The
    /// one-row placeholder (the cursor at column 2 of the caret row) and an
    /// empty composer are not. (Round-8 review, finding 7.)
    #[test]
    fn a_draft_of_more_than_one_row_is_typed() {
        let draft = |composer_rows: &[&str], row: usize, col: usize, first: usize| {
            let mut r = rows(&["⏺ Done.", "", "✻ Cogitated for 4s · done 2:41 PM", ""]);
            let top = r.len();
            r.push("─".repeat(120));
            r.extend(rows(composer_rows));
            r.push("─".repeat(120));
            r.push("  ? for shortcuts".to_string());
            Screen {
                rows: r,
                cursor_row: first + top + 1 + row,
                cursor_col: col,
                first,
                ..Screen::default()
            }
        };
        for first in [0, 30] {
            assert!(typed_draft(&draft(&["❯ Fix the parser", ""], 1, 2, first)));
            assert!(typed_draft(&draft(
                &["❯ Fix the parser", "  and the lexer"],
                1,
                2,
                first
            )));
            assert!(typed_draft(&draft(&["❯", "  and the lexer"], 1, 15, first)));
            assert!(typed_draft(&draft(&["❯ hi"], 0, 4, first)));
            assert!(!typed_draft(&draft(
                &["❯ Try \"fix lint errors\""],
                0,
                2,
                first
            )));
            assert!(!typed_draft(&draft(&["❯"], 0, 2, first)));
        }
        // A cursor on no row read is judged by its column alone.
        let mut off = draft(&["❯ Try \"fix lint errors\""], 0, 2, 0);
        off.cursor_row = 99;
        assert!(!typed_draft(&off));
    }

    // ---- the context indicator: running low, and compacted ---------------

    /// `r` with Claude Code's context indicator, `<left>% until
    /// auto-compact`, right-aligned on the row above its composer's top rule
    /// (ending two columns short of the 120-column rule, as measured).
    fn with_context(mut r: Vec<String>, left: u8) -> Vec<String> {
        let top = r
            .iter()
            .position(|row| row.starts_with('─'))
            .expect("the top rule");
        let text = format!("{left}% until auto-compact");
        let row = format!("{}{text}", " ".repeat(118 - text.chars().count()));
        r.insert(top, row);
        r
    }

    /// A worker running low, then compacting, then running low again: 15%
    /// (above the default threshold of 10), 9% and 4% while busy, the
    /// indicator gone from the busy screen (the compaction), 12% busy, and 8%
    /// at the idle point that ends the turn.
    fn descent() -> Vec<Vec<String>> {
        vec![
            with_context(busy_screen(), 15),
            with_context(busy_screen(), 9),
            with_context(busy_screen(), 4),
            busy_screen(),
            with_context(busy_screen(), 12),
            with_context(said_idle("⏺ Pushed."), 8),
        ]
    }

    /// `phase` ends with `context <n>%` while the indicator is up — after
    /// `survey 0` when the survey is open too — and is `render_phase` alone
    /// without it, a copy quoted in a tool's output included.
    #[test]
    fn phase_ends_with_the_context_left() {
        let turn = |r: Vec<String>| Turn {
            phase: worker_phase(&r),
            screen: Screen {
                rows: r,
                ..Screen::default()
            },
            timed_out: false,
        };
        let low = turn(with_context(idle_screen(), 7));
        assert_eq!(render_phase_and_survey(&low, &[]), "idle\ncontext 7%\n");
        let both = turn(with_context(with_survey(busy_screen()), 1));
        assert_eq!(
            render_phase_and_survey(&both, &[]),
            "busy\nreason status row: spinner\nsurvey 0\ncontext 1%\n"
        );
        let quoted = turn(framed(&["⏺ It reads:", "  ⎿  7% until auto-compact", ""]));
        assert_eq!(render_phase_and_survey(&quoted, &[]), "idle\n");
        let plain = turn(idle_screen());
        assert_eq!(
            render_phase_and_survey(&plain, &[]),
            render_phase(&plain, &[])
        );
    }

    /// `EVENT context` is said once a descent — at 9%, the first reading at
    /// or below 10, and not again at 4% — as the reads come, mid-turn. The
    /// indicator gone from a framed screen is the compaction, said once,
    /// and the next descent is said again, ahead of the idle point's own
    /// line. Nothing is pressed.
    #[test]
    fn watch_says_a_descent_once_and_the_compaction_once() {
        let mut m = Mock::new(true, descent());
        m.vanish_after = Some(0);
        let (lines, code) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "EVENT context seq=102 9% until auto-compact",
                "EVENT compacted seq=104",
                "EVENT context seq=106 8% until auto-compact",
                "EVENT idle seq=106 ⏺ Pushed.",
                "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        assert!(m.presses().is_empty(), "{:?}", m.requests);
    }

    /// `--context-warn 0` says neither line: the same descent and compaction
    /// — here down to 0%, which 0 would read as low if it were a threshold
    /// and not off — print only the idle point, as without the indicator.
    /// `await-turn` says nothing of the indicator, even on a session a loop
    /// left a threshold on.
    #[test]
    fn context_warn_0_says_neither_line() {
        let mut screens = descent();
        screens.insert(3, with_context(busy_screen(), 0));
        let mut m = Mock::new(true, screens);
        m.vanish_after = Some(0);
        let opts = SuperviseOpts {
            context_warn: 0,
            ..auto(30, None)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        assert_eq!(
            lines,
            [
                "EVENT idle seq=107 ⏺ Pushed.",
                "EXIT session gone (await seq 107 failed: aterm-ctl: ERR exited)",
            ]
        );

        let mut m = Mock::new(true, descent());
        let mut log: Vec<u8> = Vec::new();
        let mut s = Session::new(&mut m, None);
        s.context_warn = 10;
        let turn = s
            .await_turn_to(Duration::from_secs(30), &mut log)
            .expect("turn");
        assert_eq!(turn.phase, Phase::Idle);
        assert!(log.is_empty(), "{}", String::from_utf8_lossy(&log));
    }

    /// After `EVENT context`, a reading 30 points or more over the last one
    /// is a compaction too (29 is not); the last one, not the one warned at:
    /// 50 is 41 over the 9 but 12 over the 38 read after it. A box on the
    /// screen, which stands where the indicator is parked, is no sign of one.
    #[test]
    fn a_jump_of_30_points_is_a_compaction_and_a_box_is_not() {
        let mut m = Mock::new(
            true,
            vec![
                with_context(busy_screen(), 9),
                write_prompt(),
                with_context(busy_screen(), 38),
                with_context(busy_screen(), 50),
                with_context(busy_screen(), 80),
                with_context(said_idle("⏺ Pushed."), 60),
            ],
        );
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "EVENT context seq=101 9% until auto-compact",
                "EVENT prompt seq=102 kind=bash classify=not-read-only:rm command=rm -rf target",
                "EVENT compacted seq=105",
                "EVENT idle seq=106 ⏺ Pushed.",
                "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)",
            ]
        );
    }

    /// The indicator quoted in the transcript — the worker's words, a tool's
    /// output — is not the indicator: however low it reads, no `EVENT
    /// context`, and nothing to call a compaction when it goes.
    #[test]
    fn a_quoted_indicator_never_fires() {
        let quoted = framed(&[
            "⏺ Claude Code showed this, then compacted:",
            "  1% until auto-compact",
            "",
            "⏺ Bash(tail -2 status.txt)",
            "  ⎿  1% until auto-compact",
            "     Context left until auto-compact: 7%",
            "",
        ]);
        assert_eq!(context_left(&quoted), None);
        let mut m = Mock::new(true, vec![quoted, idle_screen()]);
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "EVENT idle seq=101 Context left until auto-compact: 7%",
                "EVENT idle seq=102 ⏺ Done.",
                "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
            ]
        );
    }

    /// `supervise` says the context lines on stderr as they come — here while
    /// the worker is busy — and never in its result, which is the idle
    /// point's.
    #[test]
    fn supervise_says_the_context_lines_on_stderr() {
        let mut m = Mock::new(
            true,
            vec![with_context(busy_screen(), 9), busy_screen(), idle_screen()],
        );
        let mut log: Vec<u8> = Vec::new();
        let (out, code) = Session::new(&mut m, Some("@s-1".to_string()))
            .supervise_to(&auto(30, None), &mut log)
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n--\n⏺ Done.\n"), "{out}");
        assert!(!out.contains("EVENT"), "{out}");
        let log = String::from_utf8(log).expect("utf-8");
        assert_eq!(
            log,
            "EVENT context seq=101 9% until auto-compact\nEVENT compacted seq=102\n"
        );
    }

    /// A peer's indicator quoted in the last transcript block — `aterm ctl
    /// @s-2 text` run on a 100-column peer, its row 82 columns in under the
    /// `⎿` gutter, ending at column 103 of this 120-column screen — with a
    /// box under the block and no status row between: the block's row, not
    /// the indicator. No `EVENT context` at the box, and no `EVENT
    /// compacted` once the box has gone and the copy with it.
    #[test]
    fn a_quoted_indicator_over_a_box_never_fires() {
        let quote = format!("     {}1% until auto-compact", " ".repeat(77));
        let boxed = bash_box(
            &["⏺ Bash(aterm ctl @s-2 text)", "  ⎿  ✢ Booping…", &quote],
            "git push",
        );
        assert_eq!(context_left(&boxed), None);
        let mut m = Mock::new(true, vec![boxed, busy_screen(), idle_screen()]);
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "EVENT prompt seq=101 kind=bash classify=not-read-only:git push command=git push",
                "EVENT idle seq=103 ⏺ Done.",
                "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
            ]
        );
    }

    /// `r` with its composer's rules `width` columns wide and the context
    /// indicator in its other spelling, `Context left until auto-compact:
    /// <left>%`, parked against their right edge: the worker's pane narrowed.
    fn narrowed(r: Vec<String>, width: usize, left: u8) -> Vec<String> {
        let mut r: Vec<String> = r
            .into_iter()
            .map(|row| {
                if row.starts_with('─') {
                    "─".repeat(width)
                } else {
                    row
                }
            })
            .collect();
        let top = r
            .iter()
            .position(|row| row.starts_with('─'))
            .expect("the top rule");
        let text = format!("Context left until auto-compact: {left}%");
        r.insert(
            top,
            format!("{}{text}", " ".repeat(width - 2 - text.chars().count())),
        );
        r
    }

    /// A pane narrowed under the watch is not a compaction: warned at 9% in
    /// 120 columns, then 55, where `Context left until auto-compact: 8%`
    /// starts at column 18 — still the indicator, against the right edge. No
    /// `EVENT compacted`, and `phase` still ends with `context 8%`.
    #[test]
    fn a_narrowed_pane_is_not_a_compaction() {
        let idle = narrowed(idle_screen(), 55, 8);
        let turn = Turn {
            phase: worker_phase(&idle),
            screen: Screen {
                rows: idle.clone(),
                ..Screen::default()
            },
            timed_out: false,
        };
        assert_eq!(render_phase_and_survey(&turn, &[]), "idle\ncontext 8%\n");
        let mut m = Mock::new(
            true,
            vec![
                with_context(busy_screen(), 9),
                narrowed(busy_screen(), 55, 8),
                idle,
            ],
        );
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "EVENT context seq=101 9% until auto-compact",
                "EVENT idle seq=103 ⏺ Done.",
                "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
            ]
        );
    }

    /// `supervise`'s watch lasts one run — each run is a process of its own,
    /// and starts armed: a run that starts with the indicator still low says
    /// `EVENT context` again at its first read, and a compaction during it is
    /// said. A compaction between two runs, while the manager acts on the
    /// first one's result, leaves the next run nothing to see it by — no
    /// indicator, as on a worker with room left — and it says nothing (the
    /// help says so; `watch` keeps one watch for as long as it runs).
    #[test]
    fn supervise_watches_one_run() {
        let run = |screens: Vec<Vec<String>>| {
            let mut m = Mock::new(true, screens);
            let mut log: Vec<u8> = Vec::new();
            let (out, code) = Session::new(&mut m, Some("@s-1".to_string()))
                .supervise_to(&auto(30, None), &mut log)
                .expect("supervise");
            assert_eq!(code, 0);
            assert!(out.starts_with("idle\n"), "{out}");
            String::from_utf8(log).expect("utf-8")
        };
        // The turn ends idle at 8%, warned at 9% on the way.
        assert_eq!(
            run(vec![
                with_context(busy_screen(), 9),
                with_context(idle_screen(), 8)
            ]),
            "EVENT context seq=101 9% until auto-compact\n"
        );
        // The next run, the indicator still low: warned again, and the
        // compaction during it said.
        assert_eq!(
            run(vec![
                with_context(busy_screen(), 8),
                busy_screen(),
                idle_screen()
            ]),
            "EVENT context seq=101 8% until auto-compact\nEVENT compacted seq=102\n"
        );
        // The next run, the worker compacted before it: nothing to say.
        assert_eq!(run(vec![busy_screen(), idle_screen()]), "");
    }

    /// The watch, read by read: the threshold is inclusive — 11 is over 10,
    /// 10 warns — and once warned, the indicator is gone only from a screen
    /// that shows the composer frame with no box up. A blank screen, a
    /// build's output, a question box that covers the composer: no
    /// compaction, and neither is 29 points over the last reading. The framed
    /// screen without it is one, and the watch is armed again: 10 warns anew.
    #[test]
    fn the_context_watch_is_inclusive_and_needs_the_frame() {
        let mut question = rows(&[
            "⏺ Which model?",
            "",
            " Select a model",
            " ❯ 1. Opus",
            "   2. Sonnet",
            "",
            " Esc to cancel",
        ]);
        question.extend(rows(&["", "  ? for shortcuts"]));
        assert!(!has_composer_frame(&question));
        assert!(parse_prompt(&question).is_some());
        let reads: Vec<(Vec<String>, &[&str])> = vec![
            (with_context(busy_screen(), 11), &[]),
            (
                with_context(busy_screen(), 10),
                &["EVENT context seq=2 10% until auto-compact"],
            ),
            (vec![String::new(); 8], &[]),
            (
                rows(&[
                    "   Compiling aterm-agent v0.84.0",
                    "    Finished `test` profile [unoptimized + debuginfo]",
                    "$ ",
                ]),
                &[],
            ),
            (question, &[]),
            (with_context(busy_screen(), 39), &[]),
            (busy_screen(), &["EVENT compacted seq=7"]),
            (
                with_context(busy_screen(), 10),
                &["EVENT context seq=8 10% until auto-compact"],
            ),
        ];
        let mut m = Mock::new(true, vec![]);
        let mut s = Session::new(&mut m, None);
        s.context_warn = 10;
        for (k, (rows, want)) in reads.into_iter().enumerate() {
            let seq = k as u64 + 1;
            let screen = Screen {
                rows,
                seq,
                ..Screen::default()
            };
            let mut said: Vec<String> = Vec::new();
            s.watch_context(&screen, &mut |line| {
                said.push(line.to_string());
                Ok(())
            })
            .expect("said");
            assert_eq!(said, want, "read {seq}");
        }
    }

    // ---- an outage: an aterm self-update's handoff ------------------------

    /// A session whose reconnect pauses are milliseconds, not seconds: 5 ms
    /// doubling to 20 ms, within `window`.
    fn quick(m: &mut Mock, window: Duration) -> Session<'_, Mock> {
        let mut s = Session::new(m, None);
        s.set_reconnect(window);
        s.pause = Duration::from_millis(5);
        s.pause_max = Duration::from_millis(20);
        s
    }
    fn watch_quick(m: &mut Mock, window: Duration) -> (Vec<String>, u8) {
        watch_quick_with(m, window, &auto(30, None))
    }
    fn watch_quick_with(m: &mut Mock, window: Duration, opts: &SuperviseOpts) -> (Vec<String>, u8) {
        let mut out: Vec<u8> = Vec::new();
        let code = quick(m, window).watch(opts, &mut out);
        let text = String::from_utf8(out).expect("utf-8");
        (text.lines().map(str::to_string).collect(), code)
    }
    /// `--auto-reads` with a budget of `ms` milliseconds.
    fn auto_ms(ms: u64, notes: Option<PathBuf>) -> SuperviseOpts {
        SuperviseOpts {
            max: Duration::from_millis(ms),
            ..auto(0, notes)
        }
    }
    /// The milliseconds a `RECONNECTED after <ms> ms` line names.
    fn reconnected_ms(line: &str) -> u128 {
        line.strip_prefix("RECONNECTED after ")
            .and_then(|r| r.strip_suffix(" ms"))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("not a RECONNECTED line: {line}"))
    }
    fn said_idle(said: &str) -> Vec<String> {
        let mut r = rows(&[said, "", "✻ Cogitated for 2s · done 2:44 PM", ""]);
        r.extend(composer("  ? for shortcuts"));
        r
    }
    /// The idle composer with a `1` typed into it: the fallback press's digit
    /// after the prompt had resolved on its own.
    fn stray_screen() -> Vec<String> {
        let mut stray = idle_screen();
        let c = stray.iter().position(|r| r == "❯").expect("composer");
        stray[c] = "❯ 1".to_string();
        stray
    }
    const CLOSED: &str = "server closed the connection without responding";

    /// A request not served — the reply never came, or the server turned the
    /// connection away before reading it — is told from a server's answer:
    /// the client's own words for a closed or cut-short exchange, a socket
    /// nothing serves or that is gone, a reset or broken connection, a socket
    /// timeout, and the two `ERR`s that turn a connection away (a full
    /// admission queue, a token that is another instance's, each a whole
    /// phrase); never another `ERR` the server sent, a usage line, a timeout
    /// the server answered, or a socket the sandbox refuses. `ERR no such
    /// session` is one more request not served only while an outage is being
    /// ridden out: the instance adopting the sessions may not host this one
    /// yet.
    #[test]
    fn a_request_not_served_is_told_from_a_server_answer() {
        let refused = "connect /d/aterm-7.sock: Connection refused (os error 61) — aterm \
                       isn't running (nothing is serving this control socket); launch \
                       aterm.app (`open -a aterm`) and retry";
        let gone = "connect /d/latest: No such file or directory (os error 2) — aterm isn't \
                    running (nothing is serving this control socket); launch aterm.app \
                    (`open -a aterm`) and retry";
        for r in [
            closed(),
            failed(1, "server hung up before the complete response"),
            failed(1, refused),
            failed(1, gone),
            failed(1, "Connection reset by peer (os error 54)"),
            failed(1, "Broken pipe (os error 32)"),
            failed(124, "Resource temporarily unavailable (os error 35)"),
            err("control server busy; retry"),
            err("auth"),
        ] {
            assert!(r.lost(), "{r:?}");
        }
        for r in [
            ok("OK\n"),
            timeout(),
            err("no such session"),
            err("exited"),
            err("halted"),
            err("authority revoked"),
            bare_err(),
            usage("text [--json] [trim]"),
            failed(
                1,
                "cannot resolve control socket: set --sock, $ATERM_CONTROL_SOCK, or \
                 $XDG_RUNTIME_DIR/$HOME",
            ),
            failed(
                1,
                "connect /d/aterm-7.sock: Operation not permitted (os error 1)",
            ),
            failed(1, "stream did not contain valid UTF-8"),
        ] {
            assert!(!r.lost(), "{r:?}");
        }
        // A note ahead of a server's answer does not make it a lost one.
        let answered = CtlReply {
            code: 1,
            stdout: String::new(),
            stderr: "note: Connection refused once, retried\naterm-ctl: ERR exited\n".to_string(),
        };
        assert!(!answered.lost());

        let mut m = Mock::new(true, vec![idle_screen()]);
        let mut s = Session::new(&mut m, None);
        assert!(
            !s.unserved(&err("no such session")),
            "outside an outage the session is gone"
        );
        s.outage = Some(Outage {
            since: Instant::now(),
            kind: "await seq".to_string(),
            told: true,
            back: false,
            pause: RECONNECT_PAUSE,
        });
        assert!(
            s.unserved(&err("no such session")),
            "in one it is not hosted yet"
        );
        assert!(!s.unserved(&err("exited")));
        assert!(!s.unserved(&err("halted")));
    }

    /// The measured failure: the connection drops under `await seq` while the
    /// worker is busy. One `RECONNECT` line however many reads fail (two here;
    /// the third, after pauses of 5 + 10 + 20 ms, answers), then `RECONNECTED
    /// after <ms> ms`, and the loop goes on — settling first, then reading —
    /// to the next review point, on the seq the new instance counts.
    #[test]
    fn watch_rides_out_a_lost_connection_to_the_next_event() {
        let mut m = Mock::new(true, vec![busy_screen(), busy_screen(), question_screen()]);
        for i in 2..=4 {
            m.by_index.insert(i, closed());
        }
        m.handoff_seq = Some(20);
        m.vanish_after = Some(0);
        let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert_eq!(
            lines[0],
            format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}")
        );
        assert!(
            reconnected_ms(&lines[1]) >= 35,
            "three pauses, 5 + 10 + 20 ms: {lines:?}"
        );
        assert_eq!(
            lines[2..],
            [
                "EVENT question seq=22 ⏺ Keep the harness or rewrite it?",
                "EXIT session gone (await seq 22 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                // The instance goes under the wait.
                "await seq 101 timeout 20000",
                // Three reads of the same session: two fail, one answers.
                "text --json tail=40",
                "text --json tail=40",
                "text --json tail=40",
                // The loop again: settle, read, report, wait past the point.
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "await seq 22 timeout 20000",
            ],
            "{:?}",
            m.requests
        );
    }

    /// The content seq starts over on the instance that adopts the session
    /// (measured: from about 652 to 12), so after the outage the wait past the
    /// point is on the NEW seq — an `await seq 101` there would never latch
    /// (the mock's server never latches it either). Nothing could be read in
    /// the outage, so the point reported before it, still showing, is
    /// reported once more: a duplicate costs the manager a look, a point
    /// missed leaves the worker waiting. The manager's turn then moves the
    /// worker, and its reply is the next EVENT.
    #[test]
    fn after_an_outage_the_point_is_reported_again_and_waited_past_on_the_new_seq() {
        let pushed = said_idle("⏺ Pushed.");
        let mut m = Mock::new(
            true,
            vec![
                idle_screen(),
                idle_screen(),
                idle_screen(),
                busy_screen(),
                pushed,
            ],
        );
        m.by_index.insert(2, closed());
        m.handoff_seq = Some(10);
        m.vanish_after = Some(0);
        let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
        assert_eq!(lines.len(), 6, "{lines:?}");
        assert_eq!(lines[0], "EVENT idle seq=101 ⏺ Done.");
        assert_eq!(
            lines[1],
            format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}")
        );
        reconnected_ms(&lines[2]);
        assert_eq!(
            lines[3..],
            [
                "EVENT idle seq=12 ⏺ Done.",
                "EVENT idle seq=14 ⏺ Pushed.",
                "EXIT session gone (await seq 14 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                "await seq 101 timeout 20000",
                "text --json tail=40",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                // The same point, reported again, waited past at the new
                // instance's seq 12.
                "await seq 12 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "await seq 13 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "await seq 14 timeout 20000",
            ],
            "{:?}",
            m.requests
        );
    }

    /// A handoff no request saw fail: the instance handing over answers the
    /// wait step (`OK timeout` — its readers parked, its count frozen) and
    /// the next request reaches the one that took over, whose count started
    /// over below the point's seq. `await seq 101` would never latch there;
    /// the read that checks the step that ran out sees seq 5 < 101 — the
    /// count started over, the screen moved — and the loop looks again, so
    /// the worker's new reply is the next EVENT. (The live repro sent `await
    /// seq 1046` until the budget ran out against a session at seq 356, its
    /// new reply never reported.)
    #[test]
    fn a_count_that_started_over_with_no_request_failing_is_seen_moved() {
        let mut m = Mock::new(true, vec![idle_screen(), said_idle("⏺ New reply on B.")]);
        m.restart = Some((2, 4));
        m.vanish_after = Some(1);
        let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
        assert_eq!(
            lines,
            [
                "EVENT idle seq=101 ⏺ Done.",
                "EVENT idle seq=6 ⏺ New reply on B.",
                "EXIT session gone (await seq 6 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                // Served, and ran out: the count is another instance's now.
                "await seq 101 timeout 20000",
                // The check: seq 5, below 101.
                "text --json tail=40",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "await seq 6 timeout 20000",
            ],
            "{:?}",
            m.requests
        );
    }

    /// A session that never comes back: every read fails until the window
    /// lapses, and the last line names the last failure.
    #[test]
    fn a_connection_that_never_comes_back_ends_when_the_window_lapses() {
        let mut m = Mock::new(true, vec![question_screen()]);
        m.down = Some((2, closed()));
        let window = Duration::from_millis(60);
        let started = Instant::now();
        let (lines, code) = watch_quick(&mut m, window);
        assert!(started.elapsed() >= window, "{:?}", started.elapsed());
        assert_eq!(
            lines,
            [
                "EVENT question seq=101 ⏺ Keep the harness or rewrite it?".to_string(),
                format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}"),
                format!(
                    "EXIT reconnect window lapsed: text --json tail=40 failed: aterm-ctl: {CLOSED}"
                ),
            ]
        );
        assert_eq!(code, 1);
        let probes = &m.requests[3..];
        assert!(!probes.is_empty(), "{:?}", m.requests);
        assert!(
            probes.iter().all(|r| r == "text --json tail=40"),
            "only reads, never the failed wait again: {:?}",
            m.requests
        );
    }

    /// An outage that keeps coming back — every read answers, every wait is
    /// dropped (the live repro dropped each `await` after 3 s) — is ONE
    /// outage: a read answering says the session answers a read, not that the
    /// wait that failed would be served now. One `RECONNECT`, one
    /// `RECONNECTED`, the relapses ridden out in silence within the one
    /// window, and the loop ends when it lapses — not a RECONNECT/RECONNECTED
    /// pair each round until the budget runs out.
    #[test]
    fn an_outage_that_keeps_coming_back_is_one_window() {
        let mut m = Mock::new(true, vec![question_screen()]);
        m.drop_awaits_from = Some(2);
        let window = Duration::from_millis(200);
        let started = Instant::now();
        let (lines, code) = watch_quick(&mut m, window);
        assert!(started.elapsed() >= window, "{:?}", started.elapsed());
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert_eq!(
            lines[0],
            "EVENT question seq=101 ⏺ Keep the harness or rewrite it?"
        );
        assert_eq!(
            lines[1],
            format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}")
        );
        reconnected_ms(&lines[2]);
        assert_eq!(
            lines[3],
            format!("EXIT reconnect window lapsed: await idle 2000 failed: aterm-ctl: {CLOSED}")
        );
        assert_eq!(code, 1);
        let relapses = m
            .requests
            .iter()
            .filter(|r| r.starts_with("await idle"))
            .count();
        assert!(
            relapses >= 2,
            "it relapsed, and said nothing more: {:?}",
            m.requests
        );
    }

    /// An outage is over once the loop gets past what failed — here the
    /// `await seq` that was lost is served again (a step that ran out) — so a
    /// later one, longer after the first than the window, is a NEW outage:
    /// said again, with a window of its own, not the first one's long run out.
    #[test]
    fn a_later_outage_is_a_new_one_with_its_own_window() {
        let mut m = Mock::new(true, vec![question_screen()]);
        m.by_index.insert(2, closed());
        m.by_index.insert(6, timeout());
        m.delay.insert(6, Duration::from_millis(400));
        m.by_index.insert(8, closed());
        m.vanish_after = Some(0);
        let (lines, code) = watch_quick(&mut m, Duration::from_millis(300));
        assert_eq!(lines.len(), 8, "{lines:?}");
        assert_eq!(
            lines[0],
            "EVENT question seq=101 ⏺ Keep the harness or rewrite it?"
        );
        assert_eq!(
            lines[1],
            format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}")
        );
        reconnected_ms(&lines[2]);
        assert_eq!(
            lines[3],
            "EVENT question seq=103 ⏺ Keep the harness or rewrite it?"
        );
        assert_eq!(
            lines[4],
            format!("RECONNECT await seq 103 failed: aterm-ctl: {CLOSED}")
        );
        assert!(reconnected_ms(&lines[5]) < 300, "its own window: {lines:?}");
        assert_eq!(
            lines[6..],
            [
                "EVENT question seq=106 ⏺ Keep the harness or rewrite it?",
                "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
    }

    /// Outside an outage the server's answer is final: `ERR no such session`
    /// under the wait ends the loop at once, with no further request. Inside
    /// one it is not yet an answer — the instance adopting the sessions may
    /// not host this one yet — so the reads go on: one that answers after it
    /// carries on, and a session that stays unknown ends the loop when the
    /// window lapses, named as gone. `--reconnect-s 0` makes a lost
    /// connection final too, as it was before.
    #[test]
    fn no_such_session_is_final_outside_an_outage_and_not_yet_inside_one() {
        let mut m = Mock::new(true, vec![question_screen()]);
        m.by_index.insert(2, err("no such session"));
        let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
        assert_eq!(
            lines,
            [
                "EVENT question seq=101 ⏺ Keep the harness or rewrite it?",
                "EXIT session gone (await seq 101 failed: aterm-ctl: ERR no such session)",
            ]
        );
        assert_eq!(code, 1);
        assert_eq!(m.requests.len(), 3, "no retry: {:?}", m.requests);

        let mut m = Mock::new(true, vec![question_screen()]);
        m.by_index.insert(2, closed());
        m.by_index.insert(3, err("no such session"));
        m.by_index.insert(4, err("no such session"));
        m.vanish_after = Some(0);
        let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert_eq!(
            lines[1],
            format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}")
        );
        reconnected_ms(&lines[2]);
        assert_eq!(
            lines[3..],
            [
                "EVENT question seq=103 ⏺ Keep the harness or rewrite it?",
                "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);

        let mut m = Mock::new(true, vec![question_screen()]);
        m.by_index.insert(2, closed());
        m.down = Some((3, err("no such session")));
        let (lines, code) = watch_quick(&mut m, Duration::from_millis(60));
        assert_eq!(
            lines,
            [
                "EVENT question seq=101 ⏺ Keep the harness or rewrite it?".to_string(),
                format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}"),
                "EXIT session gone (reconnect window lapsed: text --json tail=40 failed: \
                 aterm-ctl: ERR no such session)"
                    .to_string(),
            ]
        );
        assert_eq!(code, 1);

        let mut m = Mock::new(true, vec![question_screen()]);
        m.by_index.insert(2, closed());
        let (lines, code) = watch_quick(&mut m, Duration::ZERO);
        assert_eq!(
            lines,
            [
                "EVENT question seq=101 ⏺ Keep the harness or rewrite it?".to_string(),
                format!("EXIT await seq 101 failed: aterm-ctl: {CLOSED}"),
            ]
        );
        assert_eq!(code, 1);
        assert_eq!(m.requests.len(), 3, "{:?}", m.requests);
    }

    /// The server turning a connection away before reading it — its
    /// admission queue full (`ERR control server busy; retry`), or the token
    /// another instance's (`ERR auth`, the `latest` alias moving under the
    /// client) — served nothing: it is ridden out like a dropped connection,
    /// a probe it turns away included.
    #[test]
    fn a_connection_turned_away_is_ridden_out() {
        for text in ["control server busy; retry", "auth"] {
            let mut m = Mock::new(true, vec![question_screen()]);
            m.by_index.insert(2, err(text));
            m.by_index.insert(3, err(text));
            m.vanish_after = Some(0);
            let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
            assert_eq!(lines.len(), 5, "{text}: {lines:?}");
            assert_eq!(
                lines[1],
                format!("RECONNECT await seq 101 failed: aterm-ctl: ERR {text}")
            );
            reconnected_ms(&lines[2]);
            assert_eq!(
                lines[3..],
                [
                    "EVENT question seq=103 ⏺ Keep the harness or rewrite it?",
                    "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
                ],
                "{text}"
            );
            assert_eq!(code, 1);
        }
    }

    /// The budget bounds an outage too: once `--max-s` (or `--timeout`) is
    /// spent while a lost connection is ridden out — or as the request is
    /// lost — the loop ends as it does on any spent budget. `watch` says
    /// `TIMEOUT` and exits 124; `supervise` returns `TIMEOUT` with the last
    /// screen read; `await-turn` the timed-out turn, and when no read ever
    /// answered, a busy phase that says so.
    #[test]
    fn a_budget_spent_in_an_outage_is_the_timeout() {
        let mut m = Mock::new(true, vec![question_screen()]);
        m.down = Some((2, closed()));
        let started = Instant::now();
        let (lines, code) = watch_quick_with(&mut m, Duration::from_secs(5), &auto_ms(150, None));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(
            lines,
            [
                "EVENT question seq=101 ⏺ Keep the harness or rewrite it?".to_string(),
                format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}"),
                "TIMEOUT".to_string(),
            ]
        );
        assert_eq!(code, EXIT_TIMEOUT);

        // The budget ran out while the request that was lost was in flight.
        let mut m = Mock::new(true, vec![question_screen()]);
        m.by_index.insert(2, closed());
        m.delay.insert(2, Duration::from_millis(200));
        let (lines, code) = watch_quick_with(&mut m, Duration::from_secs(5), &auto_ms(100, None));
        assert_eq!(
            lines,
            [
                "EVENT question seq=101 ⏺ Keep the harness or rewrite it?",
                "TIMEOUT",
            ]
        );
        assert_eq!(code, EXIT_TIMEOUT);
        assert_eq!(m.requests.len(), 3, "nothing after: {:?}", m.requests);

        let mut m = Mock::new(true, vec![busy_screen()]);
        m.down = Some((2, closed()));
        let mut log: Vec<u8> = Vec::new();
        let (out, code) = quick(&mut m, Duration::from_secs(5))
            .supervise_to(&auto_ms(150, None), &mut log)
            .expect("supervise");
        assert_eq!(code, EXIT_TIMEOUT);
        assert!(
            out.starts_with("TIMEOUT\nbusy\nreason status row: spinner\n--\n"),
            "{out}"
        );
        assert!(
            String::from_utf8(log)
                .expect("utf-8")
                .starts_with("RECONNECT await seq 101 failed: "),
        );

        let mut m = Mock::new(true, vec![busy_screen()]);
        m.down = Some((0, closed()));
        let mut log: Vec<u8> = Vec::new();
        let turn = quick(&mut m, Duration::from_secs(5))
            .await_turn_to(Duration::from_millis(100), &mut log)
            .expect("turn");
        assert!(turn.timed_out);
        assert_eq!(
            render_phase(&turn, &[]),
            "busy\nreason no screen: no read answered before the timeout\n"
        );
    }

    /// A guarded press whose answer never came is not sent again as it was:
    /// the loop reads and classifies first. Here the box moved — a write now,
    /// not the read it was about to approve — so it is handed over, with the
    /// one press the log shows; nothing is APPROVED, since the press was never
    /// confirmed, and the notes say a press went unanswered. When the same
    /// read box is still up after the reconnect, it is pressed again only
    /// after the fresh read.
    #[test]
    fn a_press_in_flight_is_decided_again_from_a_fresh_read() {
        let (dir, notes) = notes_file("inflight");
        let mut m = Mock::new(true, vec![bash_one_row(), write_prompt()]);
        m.by_index.insert(2, closed());
        m.vanish_after = Some(0);
        let (lines, code) = watch_quick_with(
            &mut m,
            Duration::from_secs(5),
            &auto(30, Some(notes.clone())),
        );
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert_eq!(
            lines[0],
            format!("RECONNECT key if=Do.you.want.to.proceed 1 failed: aterm-ctl: {CLOSED}")
        );
        reconnected_ms(&lines[1]);
        assert!(
            lines[2].starts_with("EVENT prompt seq=103 kind=bash classify=not-read-only:")
                && lines[2].ends_with(" command=rm -rf target"),
            "{lines:?}"
        );
        assert_eq!(
            lines[3],
            "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)"
        );
        assert_eq!(code, 1);
        assert_eq!(m.presses(), ["key if=Do.you.want.to.proceed 1"]);
        let noted = read_notes(&dir, &notes);
        assert_eq!(noted.len(), 2, "{noted:?}");
        assert!(
            noted[0].ends_with(
                "pressed, no answer came; the box is read again after the reconnect: git log \
                 --oneline -5"
            ),
            "{noted:?}"
        );
        assert!(
            noted[1].ends_with("handed to the manager (rm): rm -rf target"),
            "{noted:?}"
        );

        let mut m = Mock::new(
            true,
            vec![
                bash_one_row(),
                bash_one_row(),
                bash_one_row(),
                busy_screen(),
                idle_screen(),
            ],
        );
        m.by_index.insert(2, closed());
        m.vanish_after = Some(0);
        let (lines, _) = watch_quick(&mut m, Duration::from_secs(5));
        assert_eq!(
            decisions(&lines[2..]),
            [
                "APPROVED git log --oneline -5",
                "EVENT idle ⏺ Done.",
                "EXIT session gone (await seq 105 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(
            m.requests[..7],
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                "key if=Do.you.want.to.proceed 1",
                "text --json tail=40",
                "await idle 2000 timeout 20000",
                // The box read and classified again, then pressed.
                "text --json tail=40",
                "key if=Do.you.want.to.proceed 1",
            ],
            "{:?}",
            m.requests
        );
    }

    /// The fallback press (a host without `key if=`) and a connection lost
    /// part way through it: what the loop knows the press did is noted as far
    /// as it is known. A `1` the server confirmed is written, lost connection
    /// or not — the settle wait lost after it, or the re-read — so it is the
    /// approval it was: `APPROVED`, noted, counted. What could not be checked
    /// after it, a digit in the composer, is checked on the first turn after
    /// the reconnect: a stray `1` there is backspaced and noted. A `key 1`
    /// whose answer never came is not an approval, and the digit it may have
    /// left is checked for the same way.
    #[test]
    fn a_fallback_press_under_a_lost_connection_is_noted_as_far_as_it_is_known() {
        // The settle wait after a confirmed `key 1` is lost; after the
        // reconnect the `1` sits in the composer.
        let (dir, notes) = notes_file("landed");
        let mut m = Mock::new(
            false,
            vec![
                bash_one_row(),
                bash_one_row(),
                stray_screen(),
                stray_screen(),
                idle_screen(),
            ],
        );
        m.by_index.insert(7, closed());
        m.vanish_after = Some(0);
        let (lines, code) = watch_quick_with(
            &mut m,
            Duration::from_secs(5),
            &auto(30, Some(notes.clone())),
        );
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert_eq!(lines[0], "APPROVED seq=102 git log --oneline -5");
        assert_eq!(
            lines[1],
            format!("RECONNECT await seq 102 failed: aterm-ctl: {CLOSED}")
        );
        reconnected_ms(&lines[2]);
        assert_eq!(
            lines[3..],
            [
                "EVENT idle seq=105 ⏺ Done.",
                "EXIT session gone (await seq 105 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        assert_eq!(
            m.presses(),
            ["key if=Do.you.want.to.proceed 1", "key 1", "key backspace"],
            "{:?}",
            m.requests
        );
        let noted = read_notes(&dir, &notes);
        assert_eq!(noted.len(), 2, "{noted:?}");
        assert!(
            noted[0].ends_with("approved read-only: git log --oneline -5"),
            "{noted:?}"
        );
        assert!(
            noted[1].ends_with(
                "backspaced a 1 left in the composer by the press for: git log --oneline -5 \
                 (the connection was lost before the press was checked)"
            ),
            "{noted:?}"
        );

        // The re-read after the settle wait is lost; after the reconnect the
        // box has taken the `1` (the worker ran, and is idle): nothing to
        // backspace.
        let (dir, notes) = notes_file("landed-clean");
        let mut m = Mock::new(
            false,
            vec![bash_one_row(), bash_one_row(), busy_screen(), idle_screen()],
        );
        m.by_index.insert(8, closed());
        m.vanish_after = Some(0);
        let (lines, _) = watch_quick_with(
            &mut m,
            Duration::from_secs(5),
            &auto(30, Some(notes.clone())),
        );
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert_eq!(lines[0], "APPROVED seq=102 git log --oneline -5");
        assert_eq!(
            lines[1],
            format!("RECONNECT text --json failed: aterm-ctl: {CLOSED}")
        );
        assert_eq!(lines[3], "EVENT idle seq=104 ⏺ Done.");
        assert_eq!(m.presses(), ["key if=Do.you.want.to.proceed 1", "key 1"]);
        let noted = read_notes(&dir, &notes);
        assert_eq!(noted.len(), 1, "{noted:?}");
        assert!(
            noted[0].ends_with("approved read-only: git log --oneline -5"),
            "{noted:?}"
        );

        // The `key 1` itself gets no answer: no approval; the digit it left
        // is found and backspaced after the reconnect.
        let (dir, notes) = notes_file("unanswered");
        let mut m = Mock::new(
            false,
            vec![
                bash_one_row(),
                bash_one_row(),
                stray_screen(),
                stray_screen(),
                idle_screen(),
            ],
        );
        m.by_index.insert(6, closed());
        m.vanish_after = Some(0);
        let (lines, _) = watch_quick_with(
            &mut m,
            Duration::from_secs(5),
            &auto(30, Some(notes.clone())),
        );
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert_eq!(
            lines[0],
            format!("RECONNECT key 1 failed: aterm-ctl: {CLOSED}")
        );
        assert_eq!(lines[2], "EVENT idle seq=105 ⏺ Done.");
        assert!(
            !lines.iter().any(|l| l.starts_with("APPROVED")),
            "{lines:?}"
        );
        assert_eq!(
            m.presses(),
            ["key if=Do.you.want.to.proceed 1", "key 1", "key backspace"]
        );
        let noted = read_notes(&dir, &notes);
        assert_eq!(noted.len(), 2, "{noted:?}");
        assert!(
            noted[0].contains("pressed, no answer came") && !noted[0].contains("approved"),
            "{noted:?}"
        );
        assert!(noted[1].contains("backspaced a 1"), "{noted:?}");
    }

    /// The dedup across an outage: a busy read before a lost connection still
    /// counts after it — the flag lives in the loop's state, not in the wait
    /// the connection dropped — so a point that looks like the one last
    /// reported is reported, not taken for it. Here the outage relapses (a
    /// relapse's probe that answers is not the outage's first — no second
    /// RECONNECTED — and clears nothing): only the busy read before the last
    /// relapse says the worker moved.
    #[test]
    fn a_busy_read_before_a_lost_connection_still_counts() {
        let mut m = Mock::new(
            true,
            vec![
                idle_screen(),
                idle_screen(),
                idle_screen(),
                idle_screen(),
                busy_screen(),
                idle_screen(),
                idle_screen(),
            ],
        );
        // Under the wait past the point; the probe answers.
        m.by_index.insert(2, closed());
        // A relapse under the next wait past it; the probe answers.
        m.by_index.insert(6, closed());
        // A relapse under the wait after the busy read (request 9).
        m.by_index.insert(10, closed());
        m.vanish_after = Some(0);
        let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
        assert_eq!(lines.len(), 6, "{lines:?}");
        assert_eq!(lines[0], "EVENT idle seq=101 ⏺ Done.");
        assert_eq!(
            lines[1],
            format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}")
        );
        reconnected_ms(&lines[2]);
        assert_eq!(
            lines[3..],
            [
                // Once more after the outage began: nothing was read in it.
                "EVENT idle seq=103 ⏺ Done.",
                // After the busy read, which the relapse did not wipe.
                "EVENT idle seq=107 ⏺ Done.",
                "EXIT session gone (await seq 107 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        assert_eq!(m.requests[9], "text --json tail=40");
        assert_eq!(m.requests[10], "await seq 105 timeout 20000");
    }

    /// `await-turn` and `supervise` ride it out the same way; their lines go
    /// to the log (stderr), and their result is untouched.
    #[test]
    fn await_turn_and_supervise_ride_it_out_with_the_lines_on_stderr() {
        let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
        m.by_index.insert(2, closed());
        m.handoff_seq = Some(7);
        let mut log: Vec<u8> = Vec::new();
        let turn = quick(&mut m, Duration::from_secs(5))
            .await_turn_to(Duration::from_secs(30), &mut log)
            .expect("turn");
        assert_eq!((turn.phase, turn.screen.seq), (Phase::Idle, 9));
        let log = String::from_utf8(log).expect("utf-8");
        let log: Vec<&str> = log.lines().collect();
        assert_eq!(log.len(), 2, "{log:?}");
        assert_eq!(
            log[0],
            format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}")
        );
        reconnected_ms(log[1]);

        let mut m = Mock::new(true, vec![busy_screen(), write_prompt()]);
        m.by_index.insert(2, closed());
        let mut log: Vec<u8> = Vec::new();
        let (out, code) = quick(&mut m, Duration::from_secs(5))
            .supervise_to(&auto(30, None), &mut log)
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(
            out.starts_with("prompt\nkind bash\ncommand rm -rf target\n"),
            "{out}"
        );
        assert!(!out.contains("RECONNECT"), "{out}");
        let log = String::from_utf8(log).expect("utf-8");
        assert!(
            log.starts_with(&format!(
                "RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}\nRECONNECTED after "
            )),
            "{log}"
        );
    }

    /// A `RECONNECT` line is cut at 160 characters like an EVENT's fields:
    /// the client's own connect error runs to about 330.
    #[test]
    fn a_reconnect_line_is_cut_at_160_characters() {
        let long = format!(
            "connect /{}/aterm.sock: No such file or directory (os error 2) — aterm isn't \
             running (nothing is serving this control socket); launch aterm.app (`open -a \
             aterm`) and retry",
            "d".repeat(120)
        );
        let mut m = Mock::new(true, vec![question_screen()]);
        m.by_index.insert(2, failed(1, &long));
        m.vanish_after = Some(0);
        let (lines, _) = watch_quick(&mut m, Duration::from_secs(5));
        assert!(
            lines[1].starts_with("RECONNECT await seq 101 failed: aterm-ctl: connect /ddd"),
            "{lines:?}"
        );
        assert_eq!(
            lines[1].chars().count(),
            "RECONNECT ".len() + LINE_CHARS,
            "{lines:?}"
        );
    }

    #[test]
    fn utc_stamp_is_iso_8601() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_stamp(1_757_527_218), "2025-09-10T18:00:18Z");
    }

    // ---- the journal (`--journal FILE`) -----------------------------------

    /// A journal file in a temp dir of this test's own.
    fn journal_file(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("aterm-journal-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("journal.jsonl");
        let _ = std::fs::remove_file(&path);
        (dir, path)
    }

    /// The records a journal file holds, and the file's mode.
    fn journal_records(path: &Path) -> (Vec<super::super::journal::JournalRecord>, u32) {
        let (records, bad) = super::super::journal::read_journal(path).expect("read the journal");
        assert_eq!(bad, 0, "every line is a record");
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
        };
        #[cfg(not(unix))]
        let mode = 0o600;
        (records, mode)
    }

    /// `watch --journal`: every line it prints is appended as one JSON object
    /// — the same scripted run as
    /// [`watch_prints_one_line_per_decision_and_keeps_watching`], so the
    /// journal's `line` fields ARE the printed lines — with the kind, phase,
    /// seq, `complete=`/`rows=` and free-text tail read from the line itself.
    /// The file is created 0600, appended to (a line already in it survives),
    /// and every record carries the `@sid` the loop was given and a Unix time
    /// that does not go backwards.
    #[test]
    fn watch_journals_every_line_it_prints() {
        let (dir, path) = journal_file("watch");
        std::fs::write(
            &path,
            "{\"t\":1,\"kind\":\"other\",\"line\":\"an older run\"}\n",
        )
        .expect("an older run's line");
        let was = journal_records(&path).1;
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                bash_one_row(),
                busy_screen(),
                question_screen(),
                busy_screen(),
                idle_screen(),
            ],
        );
        m.vanish_after = Some(3);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..auto(30, None)
        };
        let mut out: Vec<u8> = Vec::new();
        let mut warn: Vec<u8> = Vec::new();
        let code =
            Session::new(&mut m, Some("@s-1".to_string())).watch_to(&opts, &mut out, &mut warn);
        assert_eq!(code, 1);
        assert!(
            warn.is_empty(),
            "no warning: {}",
            String::from_utf8_lossy(&warn)
        );
        let printed: Vec<String> = String::from_utf8(out)
            .expect("utf-8")
            .lines()
            .map(str::to_string)
            .collect();
        let (records, mode) = journal_records(&path);
        assert_eq!(mode, was, "a file that was already there keeps its mode");
        assert_eq!(
            records[0].line, "an older run",
            "the file is appended to, never truncated"
        );
        let mine = &records[1..];
        assert_eq!(
            mine.iter().map(|r| r.line.clone()).collect::<Vec<_>>(),
            printed,
            "one record per line printed, in order"
        );
        assert_eq!(
            mine.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(),
            ["approved", "event", "event", "exit"]
        );
        assert_eq!(
            mine.iter().map(|r| r.phase.as_str()).collect::<Vec<_>>(),
            ["prompt", "question", "idle", "-"]
        );
        assert_eq!(
            mine.iter().map(|r| r.seq).collect::<Vec<_>>(),
            [Some(102), Some(104), Some(106), None]
        );
        assert_eq!(mine[0].summary, "git log --oneline -5");
        assert_eq!(mine[2].summary, "⏺ Done.");
        assert!(
            mine[3].summary.starts_with("session gone ("),
            "{:?}",
            mine[3].summary
        );
        assert!(mine.iter().all(|r| r.sid.as_deref() == Some("s-1")));
        assert!(mine.iter().all(|r| r.t > 1_700_000_000_000));
        assert!(mine.windows(2).all(|w| w[0].t <= w[1].t));
        assert!(
            mine.iter()
                .all(|r| r.complete.is_none() && r.rows.is_none())
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(dir);
    }

    /// `watch --report --journal`: the point's line carries `complete=` and
    /// `rows=`, and the record carries them as fields — with the ledger turn
    /// the report counted from, so the ledger can attribute the reply to the
    /// turn that asked for it.
    #[test]
    fn a_reported_point_journals_its_report_and_its_turn() {
        let (dir, path) = journal_file("report");
        let mut m = Mock::new(true, vec![idle_screen()]);
        m.vanish_after = Some(1);
        m.history = Some(ok(
            "turn 7 submitted=1 status=settled started_ms=1 dur_ms=2 seq=3 \
             hash=0000000000000000 arch=5:40 text=Pick%20one\n",
        ));
        m.offscreen
            .push_back(offscreen_reply(41, &["❯ Pick one"], &idle_screen()));
        let opts = SuperviseOpts {
            report: true,
            journal: Some(path.clone()),
            ..auto(30, None)
        };
        let mut out: Vec<u8> = Vec::new();
        let mut warn: Vec<u8> = Vec::new();
        Session::new(&mut m, None).watch_to(&opts, &mut out, &mut warn);
        let (records, _) = journal_records(&path);
        let point = &records[0];
        assert_eq!(point.kind, "event");
        assert_eq!(
            (point.complete, point.rows, point.turn),
            (Some(true), Some(4), Some(7))
        );
        assert_eq!(point.summary, "⏺ Done.");
        assert!(point.sid.is_none(), "no @sid was given");
        assert_eq!(records[1].kind, "exit");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(dir);
    }

    /// `supervise --journal`: the lines it says on stderr are journaled, and
    /// so are the ones `watch` would have printed for what it decides
    /// silently — the review point's EVENT line, an approval, and the
    /// TIMEOUT or `EXIT <reason>` it ends on.
    #[test]
    fn supervise_journals_what_watch_would_have_printed() {
        let (dir, path) = journal_file("supervise");
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..auto(30, None)
        };
        let mut m = Mock::new(
            true,
            vec![with_context(busy_screen(), 9), busy_screen(), idle_screen()],
        );
        let mut log: Vec<u8> = Vec::new();
        let (out, code) = Session::new(&mut m, Some("@s-1".to_string()))
            .supervise_to(&opts, &mut log)
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n"), "{out}");
        let (records, mode) = journal_records(&path);
        assert_eq!(mode, 0o600);
        assert_eq!(
            records.iter().map(|r| r.line.clone()).collect::<Vec<_>>(),
            [
                "EVENT context seq=101 9% until auto-compact",
                "EVENT compacted seq=102",
                "EVENT idle seq=103 ⏺ Done.",
            ],
            "the two lines said on stderr, then the review point"
        );
        assert_eq!(
            records.iter().map(|r| r.phase.as_str()).collect::<Vec<_>>(),
            ["context", "compacted", "idle"]
        );
        // A budget that is already spent: the TIMEOUT is the last record.
        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut log: Vec<u8> = Vec::new();
        let (_, code) = Session::new(&mut m, None)
            .supervise_to(
                &SuperviseOpts {
                    max: Duration::ZERO,
                    journal: Some(path.clone()),
                    ..SuperviseOpts::default()
                },
                &mut log,
            )
            .expect("supervise");
        assert_eq!(code, EXIT_TIMEOUT);
        let (records, _) = journal_records(&path);
        assert_eq!(records.last().map(|r| r.kind.as_str()), Some("timeout"));
        assert_eq!(records.last().map(|r| r.line.as_str()), Some("TIMEOUT"));
        // A loop that fails: the `EXIT <reason>` line `watch` would print.
        let mut m = Mock::new(true, vec![idle_screen()]);
        m.down = Some((0, err("exited")));
        let mut log: Vec<u8> = Vec::new();
        let err = Session::new(&mut m, None)
            .supervise_to(
                &SuperviseOpts {
                    journal: Some(path.clone()),
                    ..auto(30, None)
                },
                &mut log,
            )
            .expect_err("the session is gone");
        let (records, _) = journal_records(&path);
        let last = records.last().expect("a record");
        assert_eq!(last.kind, "exit");
        assert_eq!(last.line, format!("EXIT {}", exit_reason(&err)));
        assert!(
            last.line.starts_with("EXIT session gone ("),
            "{}",
            last.line
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(dir);
    }

    /// A journal that cannot be written is said ONCE and stops nothing: the
    /// loop prints every line and ends as it would have. (Opening it and
    /// appending to it fail through the same one-warning path.)
    #[test]
    fn a_journal_that_fails_is_said_once_and_stops_nothing() {
        let (dir, path) = journal_file("bad");
        // A path under a file, not a directory: the open cannot succeed.
        std::fs::write(&path, "").expect("a file where a directory would have to be");
        let opts = SuperviseOpts {
            journal: Some(path.join("under-a-file.jsonl")),
            ..auto(30, None)
        };
        let mut m = Mock::new(true, vec![busy_screen(), question_screen()]);
        m.vanish_after = Some(1);
        let mut out: Vec<u8> = Vec::new();
        let mut warn: Vec<u8> = Vec::new();
        let code = Session::new(&mut m, None).watch_to(&opts, &mut out, &mut warn);
        assert_eq!(code, 1);
        let lines: Vec<String> = String::from_utf8(out)
            .expect("utf-8")
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(
            decisions(&lines)[0],
            "EVENT question ⏺ Keep the harness or rewrite it?"
        );
        assert!(
            lines.last().is_some_and(|l| l.starts_with("EXIT ")),
            "{lines:?}"
        );
        let warn = String::from_utf8(warn).expect("utf-8");
        assert_eq!(
            warn.lines().count(),
            1,
            "one warning for the whole run: {warn:?}"
        );
        assert!(
            warn.starts_with("aterm-drive: journal ")
                && warn.contains("the loop goes on without it"),
            "{warn}"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(dir);
    }

    /// The mail lane's scripted client ([`Session::watch_mail`]'s second
    /// [`Ctl`]). `await inbox` answers come through a channel the screen mock
    /// releases into ([`Mock::release_at`]) and time out after `step` with
    /// none, as the server's parked wait does; every other request pops the
    /// next scripted reply (an empty inbox listing with none left).
    struct MailMock {
        requests: Vec<String>,
        awaits: std::sync::mpsc::Receiver<CtlReply>,
        /// The interrupter's way in: a timeout sent here ends a parked wait.
        release: std::sync::mpsc::Sender<CtlReply>,
        replies: VecDeque<CtlReply>,
        step: Duration,
    }

    /// A short parked wait: the loop's process ends within one.
    const MAIL_STEP: Duration = Duration::from_millis(10);

    impl MailMock {
        /// The lane's mock and the sender the screen mock releases through.
        fn new(replies: Vec<CtlReply>) -> (Self, std::sync::mpsc::Sender<CtlReply>) {
            let (tx, rx) = std::sync::mpsc::channel();
            (
                Self {
                    requests: Vec::new(),
                    awaits: rx,
                    release: tx.clone(),
                    replies: replies.into(),
                    step: MAIL_STEP,
                },
                tx,
            )
        }
        /// The requests with the parked waits' `timeout <ms>` and their
        /// repeats cut: what the lane asked, in order, each once.
        fn asked(&self) -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for r in &self.requests {
                let r = match r.find(" timeout ") {
                    Some(i) => r[..i].to_string(),
                    None => r.clone(),
                };
                if out.last() != Some(&r) {
                    out.push(r);
                }
            }
            out
        }
    }

    impl Ctl for MailMock {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            self.requests.push(args.join(" "));
            let verb_at = usize::from(args.first().is_some_and(|a| a.starts_with('@')));
            if args.get(verb_at) == Some(&"await") {
                return Ok(self
                    .awaits
                    .recv_timeout(self.step)
                    .unwrap_or_else(|_| timeout()));
            }
            Ok(self.replies.pop_front().unwrap_or_else(|| {
                ok("OK 0 hold=0 holder=- seen=0 bus_head=0 dropped=0 pending=0\n")
            }))
        }
        /// As the real client's: the parked wait answers at once (here with
        /// a timeout; `aterm-ctl` signalled answers with no `OK`).
        fn interrupter(&self) -> Option<Interrupter> {
            let release = self.release.clone();
            Some(Box::new(move || {
                let _ = release.send(timeout());
            }))
        }
    }

    /// The manager's inbox as the lane first reads it: one row, id 4.
    const INBOX_AT_START: &str = "OK 1 hold=0 holder=- seen=3 bus_head=80 dropped=0 pending=0\n\
                                  msg 4 off=80 t=1000 from=h-owner kind=note trust=human len=3\n";
    /// What lands during the run: the owner's ask, then the worker's report.
    const INBOX_LANDED: &str = "OK 2 hold=0 holder=- seen=3 bus_head=91 dropped=0 pending=0\n\
                                msg 5 off=90 t=2000 from=h-owner kind=ask trust=human re=70 len=7\n\
                                msg 6 off=91 t=2100 from=s-1@n-1 kind=report trust=agent len=40\n";
    const REPORT_BODY: &str = "Suite green.\n987 passed, 0 failed.\nNothing left.\n";
    const KINDS: &str = "kinds=ask,answer,task,report,note,control,ack,expired,undeliverable";

    fn mail_opts(window: Duration, grace: Duration) -> SuperviseOpts {
        SuperviseOpts {
            mail: Some(super::super::mail::MailOpts {
                inbox: None,
                report_window: window,
                idle_grace: grace,
            }),
            ..auto(30, None)
        }
    }
    fn watch_mail_lines(
        m: &mut Mock,
        lane: &mut MailMock,
        opts: &SuperviseOpts,
    ) -> (Vec<String>, u8) {
        let mut out: Vec<u8> = Vec::new();
        let mut s = Session::new(m, Some("@s-1".to_string()));
        s.set_mail_step(MAIL_STEP);
        let code = s.watch_mail(opts, Some(lane), &mut out);
        let text = String::from_utf8(out).expect("utf-8");
        (text.lines().map(str::to_string).collect(), code)
    }

    /// `watch --mail`: the lane parks on the manager's inbox and prints one
    /// `MAIL` line per delivery as it lands; the worker's `report` that
    /// follows the idle point is folded into it — ONE `EVENT turn` line for
    /// the turn, naming the report's row id and its body's row count, and
    /// no `report` is read from the screen — while the worker's socket sees
    /// exactly the requests it sees without the flag. The journal records
    /// the MAIL lines (kind `mail`) and the fold (`report`, `rows`).
    #[test]
    fn watch_mail_folds_the_workers_report_into_its_turn() {
        let (dir, path) = journal_file("mail");
        let (mut lane, release) =
            MailMock::new(vec![ok(INBOX_AT_START), ok(INBOX_LANDED), ok(REPORT_BODY)]);
        let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
        m.vanish_after = Some(1);
        m.release = Some(release);
        // Released as the idle screen is read (request 4): the report comes
        // AFTER the point.
        m.release_at.insert(4, vec![ok("OK inbox 6\n")]);
        let opts = SuperviseOpts {
            report: true,
            journal: Some(path.clone()),
            ..mail_opts(Duration::from_secs(120), Duration::from_secs(180))
        };
        let (lines, code) = watch_mail_lines(&mut m, &mut lane, &opts);
        assert_eq!(
            lines,
            [
                "MAIL id=5 off=90 from=h-owner kind=ask len=7 re=70",
                "MAIL id=6 off=91 from=s-1@n-1 kind=report len=40",
                "EVENT turn seq=102 report=6 rows=3 ⏺ Done.",
                "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        // The worker's socket: not one request more than without --mail
        // (the same sequence `watch_prints_one_line_per_decision_and_keeps_watching`
        // asserts for a point the screen never moves past), and no `report`
        // read for the folded point.
        assert_eq!(
            m.requests,
            [
                "@s-1 await gone esc.to.interrupt timeout 20000",
                "@s-1 text --json tail=40",
                "@s-1 await seq 101 timeout 20000",
                "@s-1 await idle 2000 timeout 20000",
                "@s-1 text --json tail=40",
                "@s-1 await seq 102 timeout 20000",
                "@s-1 text --json tail=40",
                "@s-1 await seq 102 timeout 20000",
            ]
        );
        assert_eq!(
            lane.asked(),
            [
                "@self inbox 1 --peek --meta".to_string(),
                format!("@self await inbox since=4 {KINDS}"),
                "@self inbox since=4 --peek --meta".to_string(),
                "@self inbox get 6".to_string(),
                format!("@self await inbox since=6 {KINDS}"),
            ]
        );
        let (records, _) = journal_records(&path);
        assert_eq!(
            records.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(),
            ["mail", "mail", "event", "exit"]
        );
        assert_eq!(
            records[1].summary,
            "id=6 off=91 from=s-1@n-1 kind=report len=40"
        );
        assert_eq!(
            (
                records[2].phase.as_str(),
                records[2].seq,
                records[2].report,
                records[2].rows,
                records[2].complete
            ),
            ("turn", Some(102), Some(6), Some(3), None)
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(dir);
    }

    /// A report that lands BEFORE the screen settles (the Stop hook fires
    /// as the turn ends) folds into the idle point that follows within the
    /// window; outside the window it is another turn's, and the idle waits
    /// its grace and is said with no report.
    #[test]
    fn a_report_that_came_first_folds_into_the_idle_that_follows() {
        let landed = "OK 1 hold=0 holder=- seen=3 bus_head=91 dropped=0 pending=0\n\
                      msg 6 off=91 t=2100 from=s-1@n-1 kind=report trust=agent len=40\n";
        let (mut lane, release) =
            MailMock::new(vec![ok(INBOX_AT_START), ok(landed), ok(REPORT_BODY)]);
        let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
        m.vanish_after = Some(1);
        m.release = Some(release);
        // Released at the loop's first request: the report is in before the
        // worker's screen was even read.
        m.release_at.insert(0, vec![ok("OK inbox 6\n")]);
        let opts = mail_opts(Duration::from_secs(120), Duration::from_secs(180));
        let (lines, _) = watch_mail_lines(&mut m, &mut lane, &opts);
        assert_eq!(
            lines,
            [
                "MAIL id=6 off=91 from=s-1@n-1 kind=report len=40",
                "EVENT turn seq=102 report=6 rows=3 ⏺ Done.",
                "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
            ]
        );
        // The window is all the loop knows when it never saw the turn begin:
        // a worker found idle at the loop's start, a report that came before
        // its screen was read. Within the window it is this turn's …
        let found_idle = |window: Duration| {
            let (mut lane, release) =
                MailMock::new(vec![ok(INBOX_AT_START), ok(landed), ok(REPORT_BODY)]);
            let mut m = Mock::new(true, vec![idle_screen()]);
            m.vanish_after = Some(1);
            m.release = Some(release);
            m.release_at.insert(0, vec![ok("OK inbox 6\n")]);
            // The report is in before the screen is read (request 1).
            m.delay.insert(1, Duration::from_millis(40));
            let opts = mail_opts(window, Duration::from_millis(50));
            watch_mail_lines(&mut m, &mut lane, &opts).0
        };
        assert_eq!(
            found_idle(Duration::from_secs(120)),
            [
                "MAIL id=6 off=91 from=s-1@n-1 kind=report len=40",
                "EVENT turn seq=101 report=6 rows=3 ⏺ Done.",
                "EXIT session gone (await seq 101 failed: aterm-ctl: ERR exited)",
            ]
        );
        // … and past it (a zero window) another turn's.
        assert_eq!(
            found_idle(Duration::ZERO),
            [
                "MAIL id=6 off=91 from=s-1@n-1 kind=report len=40",
                "EVENT idle-no-report seq=101 ⏺ Done.",
                "EXIT session gone (await seq 101 failed: aterm-ctl: ERR exited)",
            ]
        );
    }

    const LANDED_6: &str = "OK 1 hold=0 holder=- seen=3 bus_head=91 dropped=0 pending=0\n\
                            msg 6 off=91 t=2100 from=s-1@n-1 kind=report trust=agent len=40\n";
    const LANDED_7: &str = "OK 1 hold=0 holder=- seen=3 bus_head=92 dropped=0 pending=0\n\
                            msg 7 off=92 t=2200 from=s-1@n-1 kind=report trust=agent len=18\n";

    /// A worker ends a turn with a QUESTION; its Stop hook posts that turn's
    /// report (the question's text). A question point is not held, so the
    /// report is not folded and stays pending. The manager answers, the
    /// worker works and goes idle with no report of its own: the NEXT idle
    /// must not name the question turn's report as this turn's — a report
    /// from before the last point handed over is never the next turn's.
    #[test]
    fn a_question_turns_report_is_not_the_next_turns() {
        let (mut lane, release) =
            MailMock::new(vec![ok(INBOX_AT_START), ok(LANDED_6), ok(REPORT_BODY)]);
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                question_screen(),
                busy_screen(),
                idle_screen(),
            ],
        );
        m.vanish_after = Some(1);
        m.release = Some(release);
        // The hook's report lands as the question settles (request 3 is the
        // settle wait); the question is read 100 ms later (request 4), the
        // MAIL line printed by then.
        m.release_at.insert(3, vec![ok("OK inbox 6\n")]);
        m.delay.insert(4, Duration::from_millis(100));
        let opts = mail_opts(Duration::from_secs(120), Duration::from_millis(200));
        let (lines, _) = watch_mail_lines(&mut m, &mut lane, &opts);
        assert_eq!(
            lines,
            [
                "MAIL id=6 off=91 from=s-1@n-1 kind=report len=40",
                "EVENT question seq=102 ⏺ Keep the harness or rewrite it?",
                "EVENT idle-no-report seq=104 ⏺ Done.",
                "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)",
            ]
        );
    }

    /// Two reports in one turn — the hook's, then the worker's own. The
    /// first folds into the idle; the second lands while the worker is STILL
    /// idle in that turn, after the point was handed over and before the
    /// worker was read busy again. It is that turn's late report, not the
    /// next turn's.
    #[test]
    fn a_second_report_of_a_turn_is_not_the_next_turns() {
        let (mut lane, release) = MailMock::new(vec![
            ok(INBOX_AT_START),
            ok(LANDED_6),
            ok(REPORT_BODY),
            ok(LANDED_7),
            ok("Also: lint clean.\n"),
        ]);
        let mut m = Mock::new(
            true,
            vec![busy_screen(), idle_screen(), busy_screen(), idle_screen()],
        );
        m.vanish_after = Some(1);
        m.release = Some(release);
        // The hook's report as the idle is read (request 4): folds into it.
        m.release_at.insert(4, vec![ok("OK inbox 6\n")]);
        // The worker's own report after the fold, the worker still idle
        // (request 5 is the wait past the point); the next turn's reads come
        // 100 ms later, the MAIL line printed by then.
        m.release_at.insert(5, vec![ok("OK inbox 7\n")]);
        m.delay.insert(6, Duration::from_millis(100));
        let opts = mail_opts(Duration::from_secs(120), Duration::from_millis(200));
        let (lines, _) = watch_mail_lines(&mut m, &mut lane, &opts);
        assert_eq!(
            lines,
            [
                "MAIL id=6 off=91 from=s-1@n-1 kind=report len=40",
                "EVENT turn seq=102 report=6 rows=3 ⏺ Done.",
                "MAIL id=7 off=92 from=s-1@n-1 kind=report len=18",
                "EVENT idle-no-report seq=104 ⏺ Done.",
                "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)",
            ]
        );
    }

    /// A report posted mid-turn (a re-fired Stop's, a tool call's), the
    /// worker then working on past the window (100 ms here, 120 s in
    /// production): the turn DID report — the report came after the worker
    /// was read busy for it — and the line must not say it did not.
    #[test]
    fn a_mid_turn_report_older_than_the_window_is_still_its_turns() {
        let (mut lane, release) =
            MailMock::new(vec![ok(INBOX_AT_START), ok(LANDED_6), ok(REPORT_BODY)]);
        let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
        m.vanish_after = Some(1);
        m.release = Some(release);
        // Posted while busy (request 2 waits on the busy screen's seq) …
        m.release_at.insert(2, vec![ok("OK inbox 6\n")]);
        // … and the worker works on for 300 ms past the window.
        m.delay.insert(3, Duration::from_millis(300));
        let opts = mail_opts(Duration::from_millis(100), Duration::from_millis(100));
        let (lines, _) = watch_mail_lines(&mut m, &mut lane, &opts);
        assert_eq!(
            lines,
            [
                "MAIL id=6 off=91 from=s-1@n-1 kind=report len=40",
                "EVENT turn seq=102 report=6 rows=3 ⏺ Done.",
                "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
            ]
        );
    }

    /// A writer that stamps each line with when it was printed.
    struct Stamped {
        started: Instant,
        buf: Vec<u8>,
        lines: Vec<(Duration, String)>,
    }
    impl Write for Stamped {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.buf.extend_from_slice(b);
            while let Some(i) = self.buf.iter().position(|&c| c == b'\n') {
                let line = String::from_utf8_lossy(&self.buf[..i]).into_owned();
                self.lines.push((self.started.elapsed(), line));
                self.buf.drain(..=i);
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The screen is the safety net under the hold. A permission prompt that
    /// follows an idle (a queued message, a wake) is seen within one hold
    /// step, not once the grace (180 s in production) is spent: the idle is
    /// said as `idle-no-report` — the turn ended, and no report came before
    /// the worker moved on — and the prompt right after it, from the screen
    /// the hold read. A footer tick under the hold is not a move.
    #[test]
    fn a_prompt_after_an_idle_is_seen_within_a_hold_step() {
        let (mut lane, _release) = MailMock::new(vec![ok(INBOX_AT_START)]);
        let mut m = Mock::new(true, vec![busy_screen(), idle_screen(), write_prompt()]);
        m.vanish_after = Some(1);
        let opts = mail_opts(Duration::from_secs(120), Duration::from_millis(400));
        let mut out = Stamped {
            started: Instant::now(),
            buf: Vec::new(),
            lines: Vec::new(),
        };
        let mut s = Session::new(&mut m, Some("@s-1".to_string()));
        s.set_mail_step(MAIL_STEP);
        s.set_hold_step(Duration::from_millis(20));
        let _ = s.watch_mail(&opts, Some(&mut lane), &mut out);
        let lines: Vec<&str> = out.lines.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(
            lines,
            [
                "EVENT idle-no-report seq=102 ⏺ Done.",
                "EVENT prompt seq=103 kind=bash classify=not-read-only:rm command=rm -rf target",
                "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
            ]
        );
        let prompt = out
            .lines
            .iter()
            .find(|(_, l)| l.starts_with("EVENT prompt"))
            .unwrap();
        assert!(
            prompt.0 < Duration::from_millis(400),
            "the prompt waited the idle's grace: {:?}",
            out.lines
        );
        // A ticking footer (the composer's hint cycling): the same point,
        // held on.
        let (mut lane, _release) = MailMock::new(vec![ok(INBOX_AT_START)]);
        let mut ticked = rows(&["⏺ Done.", "", "✻ Cogitated for 4s · done 2:41 PM", ""]);
        ticked.extend(composer("  /help for help"));
        let mut m = Mock::new(true, vec![busy_screen(), idle_screen(), ticked]);
        m.vanish_after = Some(1);
        let opts = mail_opts(Duration::from_secs(120), Duration::from_millis(60));
        let mut out: Vec<u8> = Vec::new();
        let started = Instant::now();
        let mut s = Session::new(&mut m, Some("@s-1".to_string()));
        s.set_mail_step(MAIL_STEP);
        s.set_hold_step(Duration::from_millis(20));
        let _ = s.watch_mail(&opts, Some(&mut lane), &mut out);
        assert!(started.elapsed() >= Duration::from_millis(60));
        let text = String::from_utf8(out).expect("utf-8");
        assert_eq!(
            text.lines().next(),
            Some("EVENT idle-no-report seq=102 ⏺ Done."),
            "{text}"
        );
        assert_eq!(
            text.lines().filter(|l| l.starts_with("EVENT")).count(),
            1,
            "{text}"
        );
    }

    /// `supervise --mail`: the result IS the wake, and it is handed back the
    /// moment the review point is reached — the lane's parked `await inbox`
    /// (20 s in production, 2 s here) is cut short, not waited out.
    #[test]
    fn supervise_mails_result_does_not_wait_for_the_lanes_parked_step() {
        let (mut lane, _release) = MailMock::new(vec![ok(INBOX_AT_START)]);
        lane.step = Duration::from_secs(2);
        let mut m = Mock::new(true, vec![idle_screen()]);
        let opts = mail_opts(Duration::from_secs(120), Duration::ZERO);
        let mut log: Vec<u8> = Vec::new();
        let mut s = Session::new(&mut m, Some("@s-1".to_string()));
        s.set_mail_step(Duration::from_secs(2));
        let started = Instant::now();
        let (out, code) = s
            .supervise_with(&opts, Some(&mut lane), &mut log)
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\nreport -\n"), "{out}");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the review point was reached at once; the result came back after {:?}",
            started.elapsed()
        );
        // The cut wait is not the lane's to judge: no `MAIL lane off` line.
        assert!(log.is_empty(), "{}", String::from_utf8_lossy(&log));
    }

    /// An idle point no report comes for is held for the grace, then said
    /// as `EVENT idle-no-report` — with `--report`'s brief, since the screen
    /// is all there is to read — and the lane's parked wait re-arms on every
    /// timeout without a line. A question is not held: it prints as it
    /// always did, between the MAIL lines.
    #[test]
    fn an_idle_with_no_report_is_said_once_the_grace_is_spent() {
        let (mut lane, _release) = MailMock::new(vec![ok(INBOX_AT_START)]);
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                question_screen(),
                busy_screen(),
                idle_screen(),
            ],
        );
        m.vanish_after = Some(1);
        m.history = Some(ok(
            "turn 7 submitted=1 status=settled started_ms=1 dur_ms=2 seq=3 \
             hash=0000000000000000 arch=5:40 text=Pick%20one\n",
        ));
        m.offscreen.push_back(offscreen_reply(
            41,
            &["❯ Pick one", "⏺ Looking at both."],
            &question_screen(),
        ));
        m.offscreen
            .push_back(offscreen_reply(41, &[], &idle_screen()));
        let opts = SuperviseOpts {
            report: true,
            ..mail_opts(Duration::from_secs(120), Duration::from_millis(60))
        };
        let started = Instant::now();
        let (lines, _) = watch_mail_lines(&mut m, &mut lane, &opts);
        assert_eq!(
            lines,
            [
                "EVENT question seq=102 complete=1 rows=3 ⏺ Keep the harness or rewrite it?",
                "EVENT idle-no-report seq=104 complete=0 rows=3 ⏺ Done.",
                "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert!(
            started.elapsed() >= Duration::from_millis(60),
            "the idle waited its grace: {:?}",
            started.elapsed()
        );
        let awaits = lane
            .requests
            .iter()
            .filter(|r| r.contains("await inbox since=4"))
            .count();
        assert!(awaits >= 2, "the wait re-arms: {:?}", lane.requests);
        assert!(!lane.requests.iter().any(|r| r.contains("inbox get")));
    }

    /// Without `--mail` — the flag not given, or given with no lane to run —
    /// every line is byte-identical to `watch`'s, and the worker's socket
    /// sees the same requests.
    #[test]
    fn without_mail_every_line_is_as_before() {
        let screens = || {
            vec![
                busy_screen(),
                bash_one_row(),
                busy_screen(),
                question_screen(),
                busy_screen(),
                idle_screen(),
            ]
        };
        let mut m = Mock::new(true, screens());
        m.vanish_after = Some(1);
        let (plain, code) = watch_lines(&mut m, &auto(30, None));
        let asked = m.requests.clone();
        assert_eq!(code, 1);
        assert_eq!(plain[2], "EVENT idle seq=106 ⏺ Done.");
        // The flag, no lane.
        let mut m = Mock::new(true, screens());
        m.vanish_after = Some(1);
        let mut out: Vec<u8> = Vec::new();
        let opts = mail_opts(Duration::from_secs(120), Duration::from_secs(180));
        let code = Session::new(&mut m, None).watch_mail(&opts, None::<&mut NoLane>, &mut out);
        let text = String::from_utf8(out).expect("utf-8");
        assert_eq!(text.lines().collect::<Vec<_>>(), plain);
        assert_eq!((code, &m.requests), (1, &asked));
        // A lane, no flag.
        let (mut lane, _release) = MailMock::new(vec![ok(INBOX_AT_START)]);
        let mut m = Mock::new(true, screens());
        m.vanish_after = Some(1);
        let mut out: Vec<u8> = Vec::new();
        let code = Session::new(&mut m, Some("@s-1".to_string())).watch_mail(
            &auto(30, None),
            Some(&mut lane),
            &mut out,
        );
        let text = String::from_utf8(out).expect("utf-8");
        assert_eq!(text.lines().collect::<Vec<_>>(), plain);
        assert_eq!(code, 1);
        assert!(lane.requests.is_empty(), "no flag: the lane never runs");
        let unselected: Vec<&str> = m
            .requests
            .iter()
            .map(|r| r.strip_prefix("@s-1 ").unwrap_or(r))
            .collect();
        assert_eq!(unselected, asked);
    }

    /// A lane that cannot start (a host without the fabric verbs, no
    /// `$ATERM_PARENT_SESSION_ID` for `@self`) says so ONCE and the loop goes
    /// on: an idle point is not held, and prints as without the flag.
    #[test]
    fn a_lane_that_cannot_start_says_so_once_and_the_loop_goes_on() {
        let (mut lane, _release) = MailMock::new(vec![err("unknown verb (try: help)")]);
        let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
        m.vanish_after = Some(1);
        let opts = mail_opts(Duration::from_secs(120), Duration::from_secs(180));
        let started = Instant::now();
        let (lines, code) = watch_mail_lines(&mut m, &mut lane, &opts);
        assert_eq!(
            lines,
            [
                "MAIL lane off: inbox: ERR unknown verb (try: help) (the loop goes on without mail)",
                "EVENT idle seq=102 ⏺ Done.",
                "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "no grace was waited with no lane"
        );
        assert_eq!(lane.requests, ["@self inbox 1 --peek --meta"]);
    }

    /// `supervise --mail`: the MAIL lines go to stderr with the RECONNECT
    /// lines, the idle review point is held for the report, the result's
    /// phase lines name it (`report <id> rows=<n>`) ahead of the rows, and
    /// the journal gets the `EVENT turn` line `watch --mail` would print;
    /// with no report in the grace the result says `report -`.
    #[test]
    fn supervise_mail_holds_the_idle_and_names_the_report() {
        let (dir, path) = journal_file("supervise-mail");
        let (mut lane, release) =
            MailMock::new(vec![ok(INBOX_AT_START), ok(INBOX_LANDED), ok(REPORT_BODY)]);
        let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
        m.release = Some(release);
        m.release_at.insert(4, vec![ok("OK inbox 6\n")]);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..mail_opts(Duration::from_secs(120), Duration::from_secs(180))
        };
        let mut log: Vec<u8> = Vec::new();
        let mut s = Session::new(&mut m, Some("@s-1".to_string()));
        s.set_mail_step(MAIL_STEP);
        let (out, code) = s
            .supervise_with(&opts, Some(&mut lane), &mut log)
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\nreport 6 rows=3\n--\n"), "{out}");
        assert!(out.ends_with("? for shortcuts\n"), "{out}");
        let log = String::from_utf8(log).expect("utf-8");
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            [
                "MAIL id=5 off=90 from=h-owner kind=ask len=7 re=70",
                "MAIL id=6 off=91 from=s-1@n-1 kind=report len=40",
            ]
        );
        let (records, _) = journal_records(&path);
        assert_eq!(
            records.iter().map(|r| r.line.clone()).collect::<Vec<_>>(),
            [
                "MAIL id=5 off=90 from=h-owner kind=ask len=7 re=70",
                "MAIL id=6 off=91 from=s-1@n-1 kind=report len=40",
                "EVENT turn seq=102 report=6 rows=3 ⏺ Done.",
            ]
        );
        // No report in the grace: `report -`, and the journal's line says
        // idle-no-report.
        let (mut lane, _release) = MailMock::new(vec![ok(INBOX_AT_START)]);
        let mut m = Mock::new(true, vec![idle_screen()]);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..mail_opts(Duration::from_secs(120), Duration::from_millis(30))
        };
        let mut log: Vec<u8> = Vec::new();
        let mut s = Session::new(&mut m, Some("@s-1".to_string()));
        s.set_mail_step(MAIL_STEP);
        let (out, code) = s
            .supervise_with(&opts, Some(&mut lane), &mut log)
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\nreport -\n--\n"), "{out}");
        let (records, _) = journal_records(&path);
        assert_eq!(
            records.last().map(|r| r.line.as_str()),
            Some("EVENT idle-no-report seq=101 ⏺ Done.")
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(dir);
    }
}
