// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE GRID SPINE — harness events read from aterm's OWN view of a session,
//! with no vendor hook anywhere in the path (design
//! `docs/DESIGN-aterm-wrapper-2026-09-17.md` §0.2, §4.2, §4.7, §5.8.1).
//!
//! The central law this module exists to satisfy: **aterm's own view is the
//! spine and vendor hooks are enrichment.** Two measurements forced it, and
//! both are re-stated here because every line below answers one of them:
//!
//! * `claude --bare` removes hooks, plugins AND the statusLine in one flag
//!   (design §4.5, MEASURED) — so any capability that only works when a hook
//!   fires is off in one keystroke;
//! * the live Claude Code session hosting this work reads `detail=-` with
//!   `blocks` → `OK 0`, because an ADOPTED session owns no shell-integration
//!   block (MEASURED 2026-09-21 on this Mac: `aterm ctl @self status` →
//!   `… detail=- confidence=strong reasons=fg_job,content_activity
//!   attribution=adopted revision=172`, `aterm ctl @self blocks` → `OK 0`).
//!   A design that gated on `detail=claude` would never have armed here, and
//!   silently. [`Observer::on_sample`] therefore names the program from the
//!   grid when `detail` is absent, and the tests pin that case.
//!
//! ## Nothing samples on a clock
//!
//! There is no timer in this module and no sleep. [`Observer::read`] performs
//! the two spine reads of §4.2 — `status` (378 B, the change detector) and,
//! **only when `revision` moved**, `text --json tail=40` — and the caller
//! drives it from a pushed `subscribe` frame or an `await` that latched. The
//! revision gate is [`Observer::needs_grid`], which is
//! `aterm_link::presence::Slot::needs_screen` (presence.rs:296, "AN IDLE
//! SESSION COSTS NOTHING") restated for this crate, whose graph does not
//! reach `aterm-link`. `offscreen` and `search` are on-demand enrichment the
//! caller fetches and hands over on the [`Sample`]; nothing here fetches them
//! on a cadence. `screen` (1.76 MB, 468x `text`) is never read at all.
//!
//! ## What it reuses rather than re-deriving (design §0.1)
//!
//! | Fact | Reader |
//! |---|---|
//! | busy / prompt / limited / idle / question from one screen | `aterm_phase::worker_phase`, `busy_signal` (through it) |
//! | the limit notice and its reset text | `aterm_phase::limit_notice` |
//! | the approval box | `aterm_phase::parse_prompt` |
//! | the live zone's geometry (what is Claude Code's own row and what is transcript) | `aterm_phase::last_said_index`, `has_composer_frame` |
//! | which failure class a banner's words name | [`super::limits::banner_class`] |
//! | the `text --json` reply | [`crate::supervise::screen::parse_text_json`] |
//! | the `offscreen` reply | [`crate::supervise::report::parse_offscreen`] |
//! | the control protocol's `%XX` escape | `crate::supervise::report::pct_decode` |
//! | quoting untrusted text into a bounded row | [`super::truncate_bytes`] |
//!
//! ## Untrusted means never OBEY, not never BELIEVE (§0.2 rule 3)
//!
//! A row is data. This module quotes bounded banner text into an event so a
//! human or a ledger can read it; nothing here ever types a row back, treats
//! one as an instruction, or lets one choose an action. Every classification
//! decision is made from a CLOSED vocabulary ([`Reason`], [`BannerKind`],
//! [`SessionPhase`]) that this file owns.
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested against fixtures,
//! including one screen and one `status` line captured from a live adopted
//! Claude Code session, plus a Tier-1 conformance bind against
//! `harness_turn_observation_model` (the one law about state here: the grid
//! opens a turn, `status` alone may open but never CLOSE one). No socket is
//! opened in THIS file — and the production [`Introspect`] it used to say was
//! still coming has landed: it is [`super::wire::CtlWire`], reached by
//! `aterm harness watch`.

use super::limits::{Class, banner_class};
use super::source::{Source, SourceSet};
use super::truncate_bytes;
use crate::supervise::phase::{
    Phase, has_composer_frame, last_said_index, limit_notice, worker_phase,
};
use crate::supervise::prompt::parse_prompt;
use crate::supervise::report::{Offscreen, pct_decode};
use crate::supervise::run::CtlReply;
use crate::supervise::screen::{Screen, parse_text_json};

/// How many rows of the grid the readers are shown. The shipped choice, in
/// two places already: `supervise::run`'s `TAIL_ROWS` ("40") and
/// `aterm_link::presence::TAIL_ROWS`. The live zone is at the bottom.
pub const TAIL_ROWS: usize = 40;

/// The `status` schema this reader speaks. The catalog's own rule is to
/// "reject an unknown schema MAJOR rather than best-effort parsing", so a
/// sample carrying any other number is refused ([`ObserveError::Schema`]) —
/// the negative control, and the safe answer.
pub const SCHEMA: u64 = 1;

/// The byte cap on quoted banner text in an event.
pub const BANNER_CAP: usize = 240;

/// The byte cap on the program name an event carries.
pub const PROGRAM_CAP: usize = 64;

/// The most `reasons=` tokens kept from one `status` reply.
pub const REASONS_CAP: usize = 8;

/// The most banners one pass may emit.
pub const BANNERS_CAP: usize = 4;

// ---------------------------------------------------------------------------
// The status reply
// ---------------------------------------------------------------------------

/// aterm's own classification of the session — the `phase=` word of `status`.
/// An unknown token reads [`SessionPhase::Unknown`] rather than an error, as
/// the catalog requires ("treat an unknown phase/outcome/reason token as
/// unknown rather than an error").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionPhase {
    /// Classified, no evidence.
    #[default]
    Unknown,
    /// A program is starting.
    Starting,
    /// At a prompt, nothing running.
    Idle,
    /// A foreground job is running and content is moving.
    Running,
    /// A foreground job is running and nothing has moved for a while.
    Quiet,
    /// The session's program is gone.
    Exited,
}

impl SessionPhase {
    /// The token `status` prints.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SessionPhase::Unknown => "unknown",
            SessionPhase::Starting => "starting",
            SessionPhase::Idle => "idle",
            SessionPhase::Running => "running",
            SessionPhase::Quiet => "quiet",
            SessionPhase::Exited => "exited",
        }
    }

    /// Read one token. Anything outside the closed list is `Unknown`.
    #[must_use]
    pub fn parse(token: &str) -> SessionPhase {
        match token {
            "starting" => SessionPhase::Starting,
            "idle" => SessionPhase::Idle,
            "running" => SessionPhase::Running,
            "quiet" => SessionPhase::Quiet,
            "exited" => SessionPhase::Exited,
            _ => SessionPhase::Unknown,
        }
    }

    /// The reason token that names this phase, for an event read from
    /// `status` alone.
    fn reason(self) -> Reason {
        match self {
            SessionPhase::Unknown => Reason::StatusUnknown,
            SessionPhase::Starting => Reason::StatusStarting,
            SessionPhase::Idle => Reason::StatusIdle,
            SessionPhase::Running => Reason::StatusRunning,
            SessionPhase::Quiet => Reason::StatusQuiet,
            SessionPhase::Exited => Reason::StatusExited,
        }
    }
}

/// How sure aterm is of its own reading — the `confidence=` word, ordered so
/// a composite fact can take the weaker of two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Confidence {
    /// No evidence.
    #[default]
    Unknown,
    /// Inferred from a signal that can be wrong.
    Heuristic,
    /// Several agreeing signals.
    Strong,
    /// Read from a channel that cannot be wrong about it.
    Exact,
}

impl Confidence {
    /// The token `status` prints.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Unknown => "unknown",
            Confidence::Heuristic => "heuristic",
            Confidence::Strong => "strong",
            Confidence::Exact => "exact",
        }
    }

    /// Read one token; anything outside the closed list is `Unknown`.
    #[must_use]
    pub fn parse(token: &str) -> Confidence {
        match token {
            "heuristic" => Confidence::Heuristic,
            "strong" => Confidence::Strong,
            "exact" => Confidence::Exact,
            _ => Confidence::Unknown,
        }
    }

    /// The weaker of the two — every tie breaks toward the safe answer.
    #[must_use]
    pub fn weaker(self, other: Confidence) -> Confidence {
        if self <= other { self } else { other }
    }
}

/// How the session's program ended, when it did — the `outcome=` word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Outcome {
    /// Still running, or never classified.
    #[default]
    None,
    /// Exit status 0.
    Success,
    /// A non-zero exit status.
    Failure,
    /// Killed by a signal.
    Signal,
}

impl Outcome {
    /// The token `status` prints.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::None => "none",
            Outcome::Success => "success",
            Outcome::Failure => "failure",
            Outcome::Signal => "signal",
        }
    }

    /// Read one token; anything outside the closed list is `None`.
    #[must_use]
    pub fn parse(token: &str) -> Outcome {
        match token {
            "success" => Outcome::Success,
            "failure" => Outcome::Failure,
            "signal" => Outcome::Signal,
            _ => Outcome::None,
        }
    }
}

/// Whose consent posture the session carries — the `attribution=` word. It
/// is recorded because it EXPLAINS the case this module exists for: an
/// `adopted` session (one an aterm self-update handed over) owns no
/// shell-integration block, so `detail=` reads `-` and `blocks` answers `OK
/// 0` (MEASURED 2026-09-21; the causation is UNVERIFIED, the co-occurrence
/// is not).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Attribution {
    /// Not stated.
    #[default]
    Unknown,
    /// This aterm started the program.
    Live,
    /// A previous instance did, and handed the session over.
    Adopted,
}

impl Attribution {
    /// The token `status` prints.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Attribution::Unknown => "unknown",
            Attribution::Live => "live",
            Attribution::Adopted => "adopted",
        }
    }

    /// Read one token; anything outside the closed list is `Unknown`.
    #[must_use]
    pub fn parse(token: &str) -> Attribution {
        match token {
            "live" => Attribution::Live,
            "adopted" => Attribution::Adopted,
            _ => Attribution::Unknown,
        }
    }
}

/// One `status` reply, parsed. 378 bytes on the wire and the ONLY read this
/// module makes unconditionally: it is the change detector every other read
/// is gated on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusSample {
    /// The session id as the reply gives it.
    pub sid: String,
    /// aterm's phase word.
    pub phase: SessionPhase,
    /// How long the session has been in that phase.
    pub since_ms: Option<u64>,
    /// aterm's confidence in that phase.
    pub confidence: Confidence,
    /// The closed reason tokens, bounded to [`REASONS_CAP`].
    pub reasons: Vec<String>,
    /// The classification revision — the change detector.
    pub revision: Option<u64>,
    /// The terminal's `content_seq` as of this reply (`seq=<n>`, MEASURED on
    /// the shipped `status` line).
    ///
    /// It is read for ONE reason: `await seq <n>` is the harness's parked
    /// ingress, and `<n>` has to be a real sequence. The subscribed stream is
    /// `events`, which carries no `DELTA`, so nothing else in the loop ever
    /// learned a sequence and every arm was `await seq 0` — a predicate
    /// already true, answered `OK seq <n>` at once, which turned the parked
    /// wait into a hot loop and made every deadline unreachable.
    pub seq: Option<u64>,
    /// The sanitized running command, or `None` where the reply said `-`.
    pub detail: Option<String>,
    /// How the program ended, when it did.
    pub outcome: Outcome,
    /// The exit status, when the reply carried one.
    pub exit_code: Option<i32>,
    /// Who started the program.
    pub attribution: Attribution,
    /// A fleet halt is in force: every PTY-reaching verb answers `ERR
    /// halted`. Observation is unaffected.
    pub hold: bool,
    /// `tab_status` is on. With it off every phase reads unknown.
    pub enabled: bool,
    /// The session was ever classified. `observed=false` is NOT
    /// `phase=unknown`.
    pub observed: bool,
}

impl StatusSample {
    /// Whether `reasons=` carries a token.
    #[must_use]
    pub fn has_reason(&self, token: &str) -> bool {
        self.reasons.iter().any(|r| r == token)
    }

    /// Parse a `status` reply as a client hands it over: the `OK …` line on
    /// stdout, or on stderr behind `aterm-ctl:` as the CLI frames a
    /// status-line verb.
    pub fn parse(reply: &CtlReply) -> Result<StatusSample, ObserveError> {
        if !reply.ok() {
            return Err(ObserveError::Refused(first_line(&reply.stderr)));
        }
        let line = reply
            .stdout
            .lines()
            .chain(reply.stderr.lines())
            .map(|l| l.trim().strip_prefix("aterm-ctl:").unwrap_or(l).trim())
            .find(|l| l.starts_with("OK "))
            .ok_or_else(|| ObserveError::Malformed("status: no OK line".to_string()))?;
        StatusSample::parse_line(line)
    }

    /// Parse the `OK schema=1 …` line itself.
    pub fn parse_line(line: &str) -> Result<StatusSample, ObserveError> {
        let body = line
            .trim()
            .strip_prefix("OK ")
            .ok_or_else(|| ObserveError::Malformed("status: not an OK reply".to_string()))?;
        let fields: Vec<(&str, &str)> = body
            .split_whitespace()
            .filter_map(|tok| tok.split_once('='))
            .collect();
        let raw = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let word = |key: &str| raw(key).filter(|v| *v != "-").unwrap_or("");
        let schema = raw("schema")
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| ObserveError::Malformed("status: no schema=".to_string()))?;
        if schema != SCHEMA {
            return Err(ObserveError::Schema(schema));
        }
        let text = |key: &str, cap: usize| -> Option<String> {
            raw(key)
                .filter(|v| *v != "-" && !v.is_empty())
                .map(|v| truncate_bytes(&pct_decode(v), cap).to_string())
                .filter(|v| !v.is_empty())
        };
        let reasons = raw("reasons")
            .filter(|v| *v != "-")
            .map(|v| {
                v.split(',')
                    .filter(|t| !t.is_empty())
                    .take(REASONS_CAP)
                    .map(|t| truncate_bytes(t, 32).to_string())
                    .collect()
            })
            .unwrap_or_default();
        Ok(StatusSample {
            sid: text("sid", 64).unwrap_or_default(),
            phase: SessionPhase::parse(word("phase")),
            since_ms: raw("since_ms").and_then(|v| v.parse().ok()),
            confidence: Confidence::parse(word("confidence")),
            reasons,
            revision: raw("revision").and_then(|v| v.parse().ok()),
            seq: raw("seq").and_then(|v| v.parse().ok()),
            detail: text("detail", 128),
            outcome: Outcome::parse(word("outcome")),
            exit_code: raw("exit_code").and_then(|v| v.parse().ok()),
            attribution: Attribution::parse(word("attribution")),
            hold: raw("hold") == Some("1"),
            // Absent reads as ON: a reply that does not say is not evidence
            // that classification is off.
            enabled: raw("enabled") != Some("false"),
            observed: raw("observed") == Some("true"),
        })
    }
}

/// The `search` reply: how many times a literal has EVER appeared in this
/// session's scrollback. It corroborates a live banner and never creates one
/// (design §5.8.1 rank 1, second row).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchHits {
    /// The literal that was swept for.
    pub pattern: String,
    /// How many matches the full-history index returned.
    pub hits: usize,
}

impl SearchHits {
    /// Read a `search` reply. The CLI prints `aterm-ctl: OK <n> …` on stderr
    /// and one `<row> <col> <len>` per match on stdout; a raw client carries
    /// the header as stdout's first line. The header wins where both are
    /// present, because the rows may have been paged.
    pub fn parse(reply: &CtlReply, pattern: &str) -> Result<SearchHits, ObserveError> {
        if !reply.ok() {
            return Err(ObserveError::Refused(first_line(&reply.stderr)));
        }
        let header = reply
            .stderr
            .lines()
            .chain(reply.stdout.lines())
            .map(|l| l.trim().strip_prefix("aterm-ctl:").unwrap_or(l).trim())
            .find_map(|l| l.strip_prefix("OK ").and_then(count_head));
        let hits = match header {
            Some(n) => n,
            None => return Err(ObserveError::Malformed("search: no OK header".to_string())),
        };
        Ok(SearchHits {
            pattern: truncate_bytes(pattern, 64).to_string(),
            hits,
        })
    }
}

/// The leading decimal of an `OK <n> …` header.
fn count_head(rest: &str) -> Option<usize> {
    let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
    n.parse().ok()
}

/// The FIRST non-empty line of a message, bounded — deliberately not
/// [`super::one_line`], and named for what it does so the two are not read as
/// one function. This quotes a refusal's `stderr`, whose message is its first
/// line; folding the rest of it onto the end would pad a bounded reason
/// string with a usage banner.
fn first_line(text: &str) -> String {
    truncate_bytes(
        text.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim(),
        160,
    )
    .to_string()
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

// A `Provenance` bit-set used to live here with its own STATUS/GRID/OFFSCREEN/
// SEARCH constants and its own `names()` table — a THIRD spelling of "where
// did this fact come from", in which `grid` was a `u8` bit unrelated to the
// `grid` a window figure carried. It is now `super::source::SourceSet` over
// the one `Source` vocabulary, so an event's reads and a window's source
// print the same words from the same table.

/// Why the spine said what it said — a CLOSED vocabulary this file owns. No
/// row of any screen ever becomes a reason: a reason is a word this module
/// chose, exactly as `aterm_link::presence` publishes a word and never the
/// text it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The first sample of this session.
    FirstSample,
    /// `status detail=` named the program.
    StatusDetail,
    /// `status detail=` was `-` — the adopted case.
    DetailAbsent,
    /// Claude Code's composer frame is on the grid.
    GridComposer,
    /// Another profile's frame test named the program from the grid — an
    /// emacs mode line, say. The generic sibling of [`Reason::GridComposer`],
    /// which stays the claude-shaped spelling the ledger already carries.
    GridFrame,
    /// The program was named after the first sample, by a later grid read.
    ProgramLate,
    /// Nothing named the program.
    NoProgramEvidence,
    /// The grid's content sequence moved.
    ContentSeq,
    /// `status reasons=` carries `content_activity`.
    ContentActivity,
    /// `status revision=` moved.
    RevisionMoved,
    /// `status reasons=` carries `fg_job`.
    FgJob,
    /// The grid reads busy.
    GridBusy,
    /// An approval box is on the grid.
    GridPrompt,
    /// The grid reads idle.
    GridIdle,
    /// The grid reads limited.
    GridLimited,
    /// The grid reads question.
    GridQuestion,
    /// No grid was read this pass, so a grid-opened turn stays open.
    GridStale,
    /// `status phase=running`.
    StatusRunning,
    /// `status phase=quiet`.
    StatusQuiet,
    /// `status phase=idle`.
    StatusIdle,
    /// `status phase=starting`.
    StatusStarting,
    /// `status phase=exited`.
    StatusExited,
    /// `status phase=unknown`.
    StatusUnknown,
    /// The banner was drawn in the live zone — below the last thing said,
    /// which is where Claude Code draws its own rows.
    LiveZone,
    /// The banner came from a row that had scrolled off.
    OffscreenRow,
    /// `search` found the literal in this session's history.
    HistoryHit,
    /// The evidence window crosses a gap (`offscreen` reported `lost=` or
    /// `breaks=`), so confidence is capped.
    DegradedGap,
    /// The turn was closed because the session exited under it.
    SessionExited,
}

impl Reason {
    /// The token the ledger and `--json` print.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::FirstSample => "first_sample",
            Reason::StatusDetail => "status_detail",
            Reason::DetailAbsent => "detail_absent",
            Reason::GridComposer => "grid_composer",
            Reason::GridFrame => "grid_frame",
            Reason::ProgramLate => "program_late",
            Reason::NoProgramEvidence => "no_program_evidence",
            Reason::ContentSeq => "content_seq",
            Reason::ContentActivity => "content_activity",
            Reason::RevisionMoved => "revision_moved",
            Reason::FgJob => "fg_job",
            Reason::GridBusy => "grid_busy",
            Reason::GridPrompt => "grid_prompt",
            Reason::GridIdle => "grid_idle",
            Reason::GridLimited => "grid_limited",
            Reason::GridQuestion => "grid_question",
            Reason::GridStale => "grid_stale",
            Reason::StatusRunning => "status_running",
            Reason::StatusQuiet => "status_quiet",
            Reason::StatusIdle => "status_idle",
            Reason::StatusStarting => "status_starting",
            Reason::StatusExited => "status_exited",
            Reason::StatusUnknown => "status_unknown",
            Reason::LiveZone => "live_zone",
            Reason::OffscreenRow => "offscreen_row",
            Reason::HistoryHit => "history_hit",
            Reason::DegradedGap => "degraded:gap",
            Reason::SessionExited => "session_exited",
        }
    }
}

/// What kind of banner the grid is showing. The words are the harness's, not
/// the vendor's: the class comes from [`super::limits::banner_class`], which
/// already owns §5.8.2's literal table, so there is no second copy here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    /// A usage, session, weekly, model-bucket or spend limit.
    Limit,
    /// A login that expired or an API key that was refused.
    Auth,
    /// An approval box: the vendor owns the answer, never aterm (§0.2 rule 4).
    Permission,
    /// Capacity, network or an error this reader cannot place.
    Error,
}

impl BannerKind {
    /// The token the ledger and `--json` print.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            BannerKind::Limit => "limit",
            BannerKind::Auth => "auth",
            BannerKind::Permission => "permission",
            BannerKind::Error => "error",
        }
    }

    /// The banner kind a failure class names.
    fn of_class(class: Class) -> BannerKind {
        match class {
            Class::Session5hLimit
            | Class::Weekly7dLimit
            | Class::ModelBucketLimit
            | Class::SpendBilling => BannerKind::Limit,
            Class::Auth => BannerKind::Auth,
            Class::TransientCapacity | Class::NetworkOffline | Class::Unknown => BannerKind::Error,
        }
    }
}

/// What happened, as the spine saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// The session is running a program, and which one. `program` is `None`
    /// where nothing named it — never a guess.
    Started {
        /// The program's name, bounded to [`PROGRAM_CAP`].
        program: Option<String>,
    },
    /// A turn began.
    TurnBegan,
    /// A turn ended.
    TurnEnded,
    /// Output moved. `still_ms` is how long the session had been STILL before
    /// it moved — aterm's `since_ms` from the previous sample, where that
    /// sample was idle or quiet; `None` when nothing measured the gap.
    OutputMoved {
        /// The stillness this move ended, in milliseconds.
        still_ms: Option<u64>,
    },
    /// The session went quiet. `since_ms` is how long ago output last moved.
    Quiet {
        /// Milliseconds since the phase began.
        since_ms: Option<u64>,
    },
    /// A banner appeared. `text` is BOUNDED QUOTED DATA and is never obeyed.
    Banner {
        /// Which kind.
        kind: BannerKind,
        /// The notice as drawn, truncated to [`BANNER_CAP`].
        text: String,
        /// The reset text the notice carried, where it did. It is the
        /// notice's own words; `supervise::limit::parse_reset` places it on a
        /// clock, and this module deliberately does not, so the spine has no
        /// clock in it.
        reset: Option<String>,
    },
    /// The session's program exited.
    Exited {
        /// How it ended.
        outcome: Outcome,
        /// Its exit status, where aterm had one.
        exit_code: Option<i32>,
    },
}

impl EventKind {
    /// The token the ledger and `--json` print.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            EventKind::Started { .. } => "started",
            EventKind::TurnBegan => "turn-began",
            EventKind::TurnEnded => "turn-ended",
            EventKind::OutputMoved { .. } => "output-moved",
            EventKind::Quiet { .. } => "quiet",
            EventKind::Banner { .. } => "banner",
            EventKind::Exited { .. } => "exited",
        }
    }
}

/// One harness event: what happened, how sure aterm is, why, and from where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// What happened.
    pub kind: EventKind,
    /// aterm's confidence in it.
    pub confidence: Confidence,
    /// The named reasons, from the closed vocabulary.
    pub reasons: Vec<Reason>,
    /// Where the fact came from — the one [`Source`] vocabulary, as a set,
    /// because a turn boundary can be carried by aterm's `status` and
    /// confirmed on the parsed grid and the event says both rather than
    /// picking.
    pub sources: SourceSet,
    /// The host's clock reading, injected — this module has no clock.
    pub at_ms: u64,
    /// The `status revision=` the sample carried.
    pub revision: Option<u64>,
}

impl Event {
    /// Whether `reasons` carries `reason`.
    #[must_use]
    pub fn has_reason(&self, reason: Reason) -> bool {
        self.reasons.contains(&reason)
    }
}

// ---------------------------------------------------------------------------
// The sample and the transport
// ---------------------------------------------------------------------------

/// Everything one pass read. The machine is pure over this: production fills
/// it from `aterm-ctl`, tests fill it from fixtures, and both get the same
/// events.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sample {
    /// aterm's own classification. Always present — it is the spine.
    pub status: StatusSample,
    /// The parsed grid, present only where `revision` moved (or on the first
    /// read). `None` is not "nothing changed": it is "not looked at", and the
    /// machine treats it that way.
    pub grid: Option<Screen>,
    /// Rows a full-screen program scrolled past, where the caller fetched
    /// them. On-demand enrichment, never a per-pass read.
    pub offscreen: Option<Offscreen>,
    /// A full-scrollback sweep, where the caller asked for one.
    pub search: Option<SearchHits>,
}

/// What one [`Observer::read`] pass saw, and what it implied.
///
/// The SAMPLE is the read itself — `status` always, the grid where the
/// revision gate opened it — and the EVENTS are what the machine made of it.
/// Both, because a pass that only got the events had to read the transport a
/// second time (or tee it) to see the rows the events were derived from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpineRead {
    /// Exactly what this pass read, already parsed.
    pub sample: Sample,
    /// The events the sample implied, in the machine's fixed order.
    pub events: Vec<Event>,
}

/// Why a read could not be turned into a sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserveError {
    /// The server refused the verb (`ERR …`).
    Refused(String),
    /// The reply carried a `status` schema this reader does not speak. Fail
    /// closed: the catalog's rule is to reject an unknown MAJOR rather than
    /// parse best-effort.
    Schema(u64),
    /// The reply did not have the shape the verb documents.
    Malformed(String),
}

impl std::fmt::Display for ObserveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObserveError::Refused(why) => write!(f, "refused: {why}"),
            ObserveError::Schema(n) => write!(f, "status schema {n} is not {SCHEMA}"),
            ObserveError::Malformed(what) => write!(f, "malformed: {what}"),
        }
    }
}

/// The reads the spine makes, behind a trait so tests drive it from fixtures
/// and the host drives it from `aterm-ctl`. NO SOCKET IS OPENED IN THIS
/// MODULE, by design: the transport is the host's, and the machine above is
/// pure.
///
/// `offscreen` and `search` carry a default that answers "unsupported", so a
/// minimal host implements the two spine reads and still gets every event
/// this module can produce from them.
pub trait Introspect {
    /// `status` — 378 B, the change detector.
    fn status(&mut self) -> CtlReply;

    /// `text --json tail=<rows> trim` — the classifier read, gated by the
    /// caller on `revision` having moved.
    fn text_json_tail(&mut self, rows: usize) -> CtlReply;

    /// `offscreen since=<since> max=<max>` — rows a TUI scrolled past.
    fn offscreen(&mut self, since: u64, max: usize) -> CtlReply {
        let _ = (since, max);
        unsupported()
    }

    /// `search <pattern>` — the full-scrollback sweep.
    fn search(&mut self, pattern: &str) -> CtlReply {
        let _ = pattern;
        unsupported()
    }
}

/// The reply a default [`Introspect`] method returns.
fn unsupported() -> CtlReply {
    CtlReply {
        code: 1,
        stdout: String::new(),
        stderr: "ERR unsupported".to_string(),
    }
}

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

/// What the observer was told about the reads it makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObserveConfig {
    /// How many rows of the grid to read ([`TAIL_ROWS`]).
    pub tail_rows: usize,
}

impl Default for ObserveConfig {
    fn default() -> ObserveConfig {
        ObserveConfig {
            tail_rows: TAIL_ROWS,
        }
    }
}

// A two-variant `TurnSource` used to live here — `Grid` and `Status`, with
// its own `as_str` answering "grid" and "status". It was a SIXTH spelling of
// the one question, and its two words were already the one vocabulary's, so
// the turn source is now `Option<super::source::Source>` and the position
// admits exactly those two. The rule it carries is unchanged and is the
// whole anti-flap rule: a turn opened by the grid is closed only by the
// grid, so a pass that read no grid (because `revision` did not move) cannot
// close a live turn from `status` alone and re-open it on the next grid
// read. It is the variable the derived model
// (`aterm_spec::derive::harness_turn_observation_model`) is written over;
// the Tier-1 bind in `tests/conformance_harness.rs` projects the observer's
// state onto `turn ∈ {none, grid, status}`, which the event stream alone
// cannot give — an upgrade from `Status` to `Grid` emits no event, by design.

/// The pure state machine over injected samples.
#[derive(Debug, Clone, Default)]
pub struct Observer {
    cfg: ObserveConfig,
    seen: bool,
    exited: bool,
    program: Option<String>,
    phase: SessionPhase,
    hold: bool,
    turn: Option<Source>,
    seq: Option<u64>,
    still_ms: Option<u64>,
    grid_read: bool,
    grid_rev: Option<u64>,
    revision: Option<u64>,
    banners: Vec<(BannerKind, String)>,
}

impl Observer {
    /// A fresh observer with the shipped defaults.
    #[must_use]
    pub fn new() -> Observer {
        Observer::default()
    }

    /// A fresh observer with an explicit configuration.
    #[must_use]
    pub fn with_config(cfg: ObserveConfig) -> Observer {
        Observer {
            cfg,
            ..Observer::default()
        }
    }

    /// What it was told to read.
    #[must_use]
    pub fn config(&self) -> ObserveConfig {
        self.cfg
    }

    /// The program it has named, where anything did.
    #[must_use]
    pub fn program(&self) -> Option<&str> {
        self.program.as_deref()
    }

    /// Whether a turn is in flight.
    #[must_use]
    pub fn turn_in_flight(&self) -> bool {
        self.turn.is_some()
    }

    /// WHICH evidence opened the turn in flight, where one is.
    ///
    /// This is the anti-flap rule's own state, and it is public so the
    /// Tier-1 conformance bind can project it: a turn this answers
    /// [`Source::Grid`] for is never closed by a pass that read no grid,
    /// however `status` reads. `None` is no turn in flight.
    #[must_use]
    pub fn turn_source(&self) -> Option<Source> {
        self.turn
    }

    /// Whether the session's program has been seen to exit. Once true the
    /// `exited` event is never emitted again, and no turn is open: the exit
    /// closes one under it.
    #[must_use]
    pub fn exited(&self) -> bool {
        self.exited
    }

    /// aterm's phase as of the last sample.
    #[must_use]
    pub fn phase(&self) -> SessionPhase {
        self.phase
    }

    /// Whether a fleet halt was in force at the last sample. Observation is
    /// unaffected by it; every PTY-reaching verb is not.
    #[must_use]
    pub fn held(&self) -> bool {
        self.hold
    }

    /// The `status revision=` of the last sample.
    #[must_use]
    pub fn revision(&self) -> Option<u64> {
        self.revision
    }

    /// Whether the grid must be read again: never read, or `revision` could
    /// not be read, or it is a reading and it moved since the last grid read.
    ///
    /// This is `presence.rs`'s `Slot::needs_screen`, restated for a crate
    /// that does not depend on `aterm-link` — with ONE deliberate divergence,
    /// stated here because a silent copy would be the drift: `presence.rs`
    /// answers FALSE for a missing revision, and this answers TRUE. There the
    /// gate decides whether a cosmetic roster row is refreshed; here it
    /// decides whether [`super::watch::Watcher::prompt`] — the approval-box
    /// fence on every L3 act — is ever updated again. A `status` whose
    /// `revision=` is absent or unparseable would otherwise wedge the grid
    /// closed and freeze that fence at its last value, so the tie breaks to
    /// the safe answer: read it.
    ///
    /// Not reachable against today's server (`session_status.rs` always
    /// prints `revision=`), which is why the cost of the divergence is a read
    /// that does not happen, and its benefit is a fence that cannot freeze.
    #[must_use]
    pub fn needs_grid(&self, revision: Option<u64>) -> bool {
        match (self.grid_read, revision) {
            (false, _) => true,
            (true, None) => true,
            (true, Some(now)) => self.grid_rev != Some(now),
        }
    }

    /// ONE pass over a transport: `status` always, the grid only where
    /// [`Self::needs_grid`] says so. Called from a pushed `subscribe` frame
    /// or an `await` that latched — never on a clock.
    ///
    /// A `status` that cannot be read is an error and no events are emitted.
    /// A GRID that cannot be read is not: the spine still has `status`, the
    /// grid is simply not looked at this pass (and stays due, so the next
    /// pass retries), which is exactly the degradation the central law asks
    /// for.
    ///
    /// It hands back the [`Sample`] it built as well as the events, because a
    /// caller that wants both used to have to get them a second way: this
    /// method dropped the sample on the floor, so `watch` wrapped the
    /// transport in a tee that CLONED both replies and re-parsed them. The
    /// sample is already here and already parsed; returning it deleted that
    /// wrapper and one whole re-parse of the grid per pass.
    pub fn read(
        &mut self,
        src: &mut dyn Introspect,
        at_ms: u64,
    ) -> Result<SpineRead, ObserveError> {
        let status = StatusSample::parse(&src.status())?;
        let mut grid = None;
        if self.needs_grid(status.revision) {
            let reply = src.text_json_tail(self.cfg.tail_rows);
            if reply.ok()
                && let Ok(screen) = parse_text_json(&reply.stdout)
            {
                self.grid_read = true;
                self.grid_rev = status.revision;
                grid = Some(screen);
            }
        }
        let sample = Sample {
            status,
            grid,
            offscreen: None,
            search: None,
        };
        let events = self.on_sample(&sample, at_ms);
        Ok(SpineRead { sample, events })
    }

    /// The machine itself: one sample in, the events it implies out, in a
    /// fixed order — `started`, `output-moved`, `turn-began`, `banner`,
    /// `turn-ended`, `quiet`, `exited`.
    ///
    /// Everything is derived from aterm's own view. With ZERO vendor hooks
    /// every event above is still produced; hooks would add exactness to
    /// some of them and create none of them.
    pub fn on_sample(&mut self, sample: &Sample, at_ms: u64) -> Vec<Event> {
        let st = &sample.status;
        let mut out = Vec::new();
        let rev = st.revision;
        let first = !self.seen;
        let degraded = sample
            .offscreen
            .as_ref()
            .is_some_and(|o| o.lost > 0 || o.breaks > 0);
        let rows = sample.grid.as_ref().map(|g| g.rows.as_slice());
        let mut push = |kind: EventKind,
                        confidence: Confidence,
                        mut reasons: Vec<Reason>,
                        sources: SourceSet| {
            let confidence = if degraded {
                reasons.push(Reason::DegradedGap);
                confidence.weaker(Confidence::Strong)
            } else {
                confidence
            };
            out.push(Event {
                kind,
                confidence,
                reasons,
                sources,
                at_ms,
                revision: rev,
            });
        };

        // 1. STARTED — the first sample, and once more if a later grid names
        //    a program the first sample could not. That second emission is
        //    the adopted case's own path: `detail=-` names nothing, and the
        //    composer frame on the grid does.
        //    A program already named is never re-derived: the frame test is
        //    only run while the answer is still open.
        if !self.seen {
            self.seen = true;
            let named = program_of(st, rows);
            self.program = named.program.clone();
            let mut reasons = vec![Reason::FirstSample, named.reason];
            if st.has_reason("fg_job") {
                reasons.push(Reason::FgJob);
            }
            push(
                EventKind::Started {
                    program: named.program,
                },
                named.confidence,
                reasons,
                named.sources.with(Source::Status),
            );
        } else if self.program.is_none() {
            let named = program_of(st, rows);
            if let Some(program) = named.program {
                self.program = Some(program.clone());
                push(
                    EventKind::Started {
                        program: Some(program),
                    },
                    named.confidence,
                    vec![Reason::ProgramLate, named.reason],
                    named.sources,
                );
            }
        }

        // 2. OUTPUT MOVED — the grid's content counter is aterm's own and
        //    cannot be wrong about a write; `status` is the fallback where
        //    the grid was not read this pass.
        // `seq` is PER-GRID: an alt-screen round trip does not move the MAIN
        // grid's counter (the catalog's own caveat, carried in design §4.2).
        // So a counter that held still does not VETO the status reading — it
        // costs it exactness, and the reasons say which evidence is left.
        let status_moved =
            !first && rev.is_some() && rev != self.revision && st.has_reason("content_activity");
        let moved = match (sample.grid.as_ref(), self.seq) {
            (Some(g), Some(prev)) if g.seq > prev => Some((
                Confidence::Exact,
                vec![Reason::ContentSeq],
                SourceSet::just(Source::Grid),
            )),
            _ if status_moved => Some((
                st.confidence.weaker(if sample.grid.is_some() {
                    Confidence::Heuristic
                } else {
                    Confidence::Strong
                }),
                vec![Reason::RevisionMoved, Reason::ContentActivity],
                SourceSet::just(Source::Status),
            )),
            _ => None,
        };
        if let Some((confidence, reasons, sources)) = moved {
            push(
                EventKind::OutputMoved {
                    still_ms: self.still_ms,
                },
                confidence,
                reasons,
                sources,
            );
        }

        // 3/5. THE TURN. Grid evidence decides where there is any; `status`
        //      may OPEN a turn when none is open and may CLOSE only one it
        //      opened itself.
        let (in_flight, reason, confidence, sources, source) = match rows {
            Some(r) => {
                let phase = worker_phase(r);
                // A turn blocked on an approval box has NOT ended: it is
                // waiting on a human. The safe answer keeps it open.
                (
                    matches!(phase, Phase::Busy | Phase::Prompt),
                    grid_reason(&phase),
                    Confidence::Strong,
                    SourceSet::just(Source::Grid),
                    Source::Grid,
                )
            }
            None => (
                st.phase == SessionPhase::Running,
                st.phase.reason(),
                st.confidence.weaker(Confidence::Heuristic),
                SourceSet::just(Source::Status),
                Source::Status,
            ),
        };
        // A status-sourced transition taken while an older grid reading
        // exists says so, rather than leaving the weaker evidence unexplained.
        let mut reasons = vec![reason];
        if source == Source::Status && self.grid_read {
            reasons.push(Reason::GridStale);
        }
        let mut ended: Option<(Confidence, Vec<Reason>, SourceSet)> = None;
        match (self.turn, in_flight) {
            (None, true) => {
                self.turn = Some(source);
                push(EventKind::TurnBegan, confidence, reasons, sources);
            }
            (Some(Source::Grid), false) if source == Source::Status => {
                // A grid-opened turn is NEVER closed from `status` alone:
                // this pass simply did not look at the grid.
            }
            (Some(_), false) => {
                self.turn = None;
                ended = Some((confidence, reasons, sources));
            }
            (Some(_), true) => {
                // A grid reading upgrades the source of an open turn, so a
                // status-opened turn can later be closed by the grid.
                if source == Source::Grid {
                    self.turn = Some(Source::Grid);
                }
            }
            (None, false) => {}
        }

        // 4. BANNERS — live-zone rows first, then the rows a repaint took
        //    away. `search` corroborates and never creates.
        let live = rows.map(banners_of).unwrap_or_default();
        let mut seen_now: Vec<(BannerKind, String)> = Vec::new();
        for (kind, text, reset, reason) in live
            .into_iter()
            .chain(offscreen_banners(sample.offscreen.as_ref()))
            .take(BANNERS_CAP)
        {
            let key = (kind, text.clone());
            if seen_now.contains(&key) {
                continue;
            }
            seen_now.push(key.clone());
            if self.banners.contains(&key) {
                continue;
            }
            let mut reasons = vec![reason];
            let mut sources = if reason == Reason::OffscreenRow {
                SourceSet::just(Source::Offscreen)
            } else {
                SourceSet::just(Source::Grid)
            };
            let mut confidence = if reason == Reason::OffscreenRow {
                Confidence::Heuristic
            } else {
                Confidence::Strong
            };
            if sample.search.as_ref().is_some_and(|s| s.hits > 0) {
                reasons.push(Reason::HistoryHit);
                sources = sources.with(Source::Search);
                confidence = confidence.max(Confidence::Strong);
            }
            push(
                EventKind::Banner { kind, text, reset },
                confidence,
                reasons,
                sources,
            );
        }
        self.banners = seen_now;

        // 5. TURN ENDED, after the banner that explains it.
        if let Some((confidence, reasons, sources)) = ended {
            push(EventKind::TurnEnded, confidence, reasons, sources);
        }

        // 6. QUIET — aterm's own word for "output stopped moving".
        if st.phase == SessionPhase::Quiet && self.phase != SessionPhase::Quiet {
            push(
                EventKind::Quiet {
                    since_ms: st.since_ms,
                },
                st.confidence,
                vec![Reason::StatusQuiet],
                SourceSet::just(Source::Status),
            );
        }

        // 7. EXITED — once, with any open turn closed under it first.
        if st.phase == SessionPhase::Exited && !self.exited {
            self.exited = true;
            if self.turn.take().is_some() {
                push(
                    EventKind::TurnEnded,
                    st.confidence,
                    vec![Reason::SessionExited],
                    SourceSet::just(Source::Status),
                );
            }
            push(
                EventKind::Exited {
                    outcome: st.outcome,
                    exit_code: st.exit_code,
                },
                st.confidence,
                vec![Reason::StatusExited],
                SourceSet::just(Source::Status),
            );
        }

        // Carry the sample forward.
        self.phase = st.phase;
        self.hold = st.hold;
        self.revision = rev;
        if let Some(g) = sample.grid.as_ref() {
            self.seq = Some(g.seq);
        }
        self.still_ms = match st.phase {
            SessionPhase::Idle | SessionPhase::Quiet => st.since_ms,
            _ => None,
        };
        out
    }
}

/// What named the program, and how sure that is.
struct Named {
    program: Option<String>,
    reason: Reason,
    confidence: Confidence,
    sources: SourceSet,
}

/// Name the program from aterm's own view. `detail=` where the session has a
/// shell-integration block; a PROFILE's frame test over the grid where it
/// does not — the ADOPTED case, which is the one that killed the first
/// design.
///
/// The frame tests are [`super::profile`]'s, not this module's: claude's
/// composer frame and emacs' mode line today, and codex deliberately has none
/// because nothing on its real screen names it without guessing. The
/// confidence stays heuristic for all of them — a frame is evidence, never
/// proof.
fn program_of(st: &StatusSample, rows: Option<&[String]>) -> Named {
    if let Some(detail) = &st.detail {
        return Named {
            program: Some(truncate_bytes(detail, PROGRAM_CAP).to_string()),
            reason: Reason::StatusDetail,
            confidence: st.confidence,
            sources: SourceSet::just(Source::Status),
        };
    }
    if let Some((profile, _)) = super::profile::identify(None, rows.unwrap_or(&[])) {
        return Named {
            program: Some(profile.id.to_string()),
            reason: if profile.id == "claude" {
                Reason::GridComposer
            } else {
                Reason::GridFrame
            },
            confidence: Confidence::Heuristic,
            sources: SourceSet::just(Source::Grid).with(Source::Status),
        };
    }
    Named {
        program: None,
        reason: if st.detail.is_none() && rows.is_some() {
            Reason::NoProgramEvidence
        } else {
            Reason::DetailAbsent
        },
        confidence: Confidence::Unknown,
        sources: SourceSet::just(Source::Status),
    }
}

/// The reason token a worker phase names.
fn grid_reason(phase: &Phase) -> Reason {
    match phase {
        Phase::Busy => Reason::GridBusy,
        Phase::Prompt => Reason::GridPrompt,
        Phase::Limited { .. } => Reason::GridLimited,
        Phase::Question => Reason::GridQuestion,
        Phase::Idle => Reason::GridIdle,
    }
}

/// The glyph Claude Code opens a TOOL RESULT block with. Rows from it to the
/// next blank or un-indented row are the tool's own output, quoted by the
/// vendor, and [`banners_of`]'s free scan skips them: what a command prints
/// is not something the vendor said.
const TOOL_RESULT_GLYPH: char = '⎿';

/// Every banner the LIVE ZONE shows, read through the shipped readers.
///
/// The live zone is the rows below the last thing said — where Claude Code
/// draws its OWN rows — which is `aterm_phase`'s own rule for
/// `banner_limit`, not a new one. It is what keeps a transcript row that
/// merely QUOTES `Please run /login` (this very session's screen quotes
/// several of §5.8.2's literals) from reading as a live banner.
fn banners_of(rows: &[String]) -> Vec<(BannerKind, String, Option<String>, Reason)> {
    let mut out = Vec::new();
    if let Some((text, reset)) = limit_notice(rows) {
        let kind = banner_class(&text).map_or(BannerKind::Limit, BannerKind::of_class);
        out.push((
            kind,
            truncate_bytes(&text, BANNER_CAP).to_string(),
            reset,
            Reason::LiveZone,
        ));
    }
    if let Some(prompt) = parse_prompt(rows) {
        let text = if prompt.command.is_empty() {
            prompt.kind.name().to_string()
        } else {
            format!("{}: {}", prompt.kind.name(), prompt.command)
        };
        out.push((
            BannerKind::Permission,
            truncate_bytes(&text, BANNER_CAP).to_string(),
            None,
            Reason::LiveZone,
        ));
    }
    // Where nothing was said AND there is no composer frame, there is no live
    // zone to appeal to — every row is unplaced text — so the scan does not
    // run at all. The tie breaks toward "not a banner".
    let Some(from) = (match last_said_index(rows) {
        Some(i) => Some(i + 1),
        None => has_composer_frame(rows).then_some(0),
    }) else {
        return out;
    };
    let mut in_tool_result = false;
    for row in rows.iter().skip(from) {
        let t = row.trim();
        if t.is_empty() {
            in_tool_result = false;
            continue;
        }
        // TOOL OUTPUT IS NOT A BANNER. `⎿` opens a tool-result block and the
        // indented rows under it are the tool's own bytes, quoted by the
        // vendor — `cat` a file, `grep` a changelog, print a build log, and
        // whatever words land there are the COMMAND's, not the vendor's. A
        // banner needle matched inside that block mints a limit class from
        // in-session, attacker-or-accident-writable text, and a class
        // short-circuits `classify_liveness`, which turns the stall ladder
        // off for the class's lifetime. The block ends at the first blank or
        // un-indented row, which is where the vendor's own rows resume.
        if t.starts_with(TOOL_RESULT_GLYPH) {
            in_tool_result = true;
            continue;
        }
        if in_tool_result {
            if row.starts_with(' ') || row.starts_with('\t') {
                continue;
            }
            in_tool_result = false;
        }
        let Some(class) = banner_class(t) else {
            continue;
        };
        let kind = BannerKind::of_class(class);
        // A limit the shipped reader already placed is not repeated here.
        if kind == BannerKind::Limit && out.iter().any(|(k, _, _, _)| *k == BannerKind::Limit) {
            continue;
        }
        out.push((
            kind,
            truncate_bytes(t, BANNER_CAP).to_string(),
            None,
            Reason::LiveZone,
        ));
    }
    out
}

/// Banners on rows that a full-screen repaint took away. There is no live
/// zone to appeal to in an archive, so these are HEURISTIC and never more:
/// the safe answer for a row whose position can no longer be checked.
fn offscreen_banners(off: Option<&Offscreen>) -> Vec<(BannerKind, String, Option<String>, Reason)> {
    let Some(off) = off else {
        return Vec::new();
    };
    off.archived
        .iter()
        .filter_map(|row| {
            let t = row.trim();
            banner_class(t).map(|class| {
                (
                    BannerKind::of_class(class),
                    truncate_bytes(t, BANNER_CAP).to_string(),
                    None,
                    Reason::OffscreenRow,
                )
            })
        })
        .take(BANNERS_CAP)
        .collect()
}

#[path = "observe_tests.rs"]
#[cfg(test)]
mod tests;
