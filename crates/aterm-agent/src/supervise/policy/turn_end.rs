// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TURN-END DECIDER: what the supervisor does when a worker's turn has
//! ended — nothing, wait, type a continuation, accept the worker's own
//! suggestion, type a slash command, or hand the point to a person. One pure
//! function, [`decide_turn_end`], over what the screen says
//! ([`TurnEndReading`], from `aterm-phase`'s reader) and what this policy
//! remembers of the session ([`TurnEndState`]); the loop only executes what
//! it returns (`supervise/turn_end_loop.rs`) and tells the state what it did
//! ([`TurnEndState::acted`]) and what the next point showed
//! ([`TurnEndState::observe`]). Every switch is the owner's `[harness]` table
//! ([`SupervisorConfig`]).
//!
//! **Fully automatic** (owner, 2026-09-24: *"UNLESS aterm is configured
//! otherwise, it is in FULLY AUTOMATIC mode and there cannot be any
//! interruptions because there is nobody to address the interruption"*).
//! Every point where a turn ended gets an automatic answer, and a person is
//! told only of what the owner's `[harness]` limited, or what cannot be
//! answered at all:
//!
//! * **Continue** (owner decision 2 — the harness audit's first blocker: 60%
//!   of the owner's typed prompts were a rote `keep going`). At an
//!   authoritative idle point, or a question that is an offer (`want me
//!   to…`), with no box, no wall and nothing typed in the composer: accept
//!   the worker's own suggestion when it is a continuation
//!   ([`suggestion_allowed`]), else type `cfg.continue_text` (the standing
//!   rules with it when a rules file is set).
//! * **Answer** ([`RULE_ANSWER`]). A worker that asks a person — a stop phrase
//!   ([`STOP_PHRASES`]), a choice between listed options, any other question
//!   — gets `cfg.answer_text` (decide yourself, prefer reversible steps, keep
//!   going). One that names an irreversible act — an offer to do something
//!   destructive ([`DESTRUCTIVE_WORDS`]), a question or a choice naming one
//!   anywhere ([`Said::Irreversible`]) — gets [`REVERSIBLE_ANSWER`] instead,
//!   whatever `answer_text` says: nobody is there to approve the act, and "take
//!   the option you would recommend" may be it (D1). Under `answer_questions =
//!   false` either is escalated instead.
//! * **A growing back-off, never "done"**. A turn that did less than
//!   [`TurnEndTiming::min_work`] of busy work is SHORT, and so is the first
//!   point a supervisor sees (its work unknown) — and so is the yield of a
//!   continuation or an answer that ends with the worker saying it is DONE
//!   ([`says_done`]: `nothing left to do`, `all done`, `what would you like
//!   me to work on?`), however long it worked: a worker that answers every
//!   `keep going` by re-running its checks for three minutes was continued
//!   at once, for ever (the hazards review of 2026-09-25). Each short turn
//!   in a row doubles the wait before the next act, from
//!   [`TurnEndTiming::short_backoff`] up to [`TurnEndTiming::
//!   short_backoff_max`] (2 min, 4, 8 … 60); a turn of real work that does
//!   not end done ends the streak, and the next point is answered at once.
//!   (Until 2026-09-24 a short turn was left alone and two in a row
//!   escalated "worker reports done".) `cfg.continue_per_hour` is the
//!   owner's cap: `0`, the default, is none; past a written cap the point is
//!   escalated.
//! * **No turn, no turn end.** A session nobody has asked anything
//!   ([`TurnEndReading::fresh`]: the launch card and no message) is waiting
//!   for its first task, not stopped short: nothing is typed into it, and
//!   its first point starts no streak (the E2E probe of 2026-09-25: a
//!   brand-new session got `keep going` two minutes after its launch).
//! * **A person wins.** Nothing is typed within `cfg.human_grace_s` of a
//!   person's last keystroke into the session ([`TurnEndReading::person`]:
//!   the server's `human_ms=`, or the draft in the composer last changing,
//!   whichever is later), and a turn a person stopped with Esc (`⎿
//!   Interrupted · What should Claude do instead?`) is continued only once
//!   that grace has passed since the point showed.
//! * **A draft left standing is sent** ([`TurnEndAction::Submit`]). Text in
//!   the composer that has not changed for the grace, with nobody typing,
//!   is the next message: where the policy would type its act, it submits
//!   the draft instead, under that act's rule — never types over it, and
//!   never leaves a fully automatic session stopped on it with nobody told
//!   (until 2026-09-24 it was left for ever).
//! * **An act the worker never takes is acted again.** A text act whose
//!   `❯` row stays unanswered, no spinner drawn, for
//!   [`TurnEndTiming::take_within`] is the short yield it is — the next act
//!   goes on the back-off ([`TurnEndState::observe`]); it was escalated as
//!   "not taken" until 2026-09-24.
//!
//! **Walls** ([`aterm_phase::wall`]). An API error or an overload (retryable
//! or not): wait out [`TurnEndTiming::retry_backoff`] (1, 5, 15, 30, 60 min;
//! the last for every retry after) from each appearance and continue after
//! each, for ever. A usage window or a spend limit: waited out, handled and
//! nobody told, when the vendor says it goes on by itself (`continuing
//! automatically at …`) — continued only if it has not gone on a
//! [`TurnEndTiming::reset_grace`] past its reset — and otherwise a usage
//! window or a spend limit is continued a [`TurnEndTiming::reset_grace`]
//! past the reset it names — never anything bought — and after a
//! continuation that hit the wall again, [`TurnEndTiming::limit_backoff`]. A
//! model bucket (owner decision 3): the agent RELAUNCHED on the fallback
//! model ([`Restart::Model`], `--model <fallback>` on the relaunch line,
//! session-only), and relaunched on the bucket's model again at the
//! bucket's reset ([`Restart::ModelBack`]) — never Claude's own `/model`,
//! which also saves the person's default for every new session (measured
//! on 2.1.282; D7 of the reconciliation of 2026-09-25). With no fallback,
//! once switched, or where no host relaunches it (`drive watch`), the reset
//! is waited out. A bucket that asks
//! CONSENT to go on on usage credits is accepted (owner, 2026-09-24: *"a
//! model/extra-usage consent dialog → accept"*): continued, so the vendor's
//! confirm comes up as a box, which the approval policy answers with its
//! yes — and only when that continuation hits the bucket again is the model
//! switched as above. Nothing is ever BOUGHT (the approval policy's
//! `buys`). A full context: `/compact`, then continue (again on the retry
//! ladder). A lost login: `/login`, then the ONE irreducible escalation —
//! "finish sign-in in the browser". Claude Code's critical-memory banner
//! ([`WallKind::Memory`], read even under a running spinner): the agent is
//! RESTARTED — ended and relaunched on its own conversation by the session's
//! host, which then tells it to carry on ([`TurnEndAction::Restart`], D3 of
//! the reconciliation of 2026-09-25) — ahead of every gate a typed act waits
//! on, since nothing typed into a process past saving is read. Escalated with
//! its remedy only where the restart cannot be made (no host restarts here,
//! or `[harness] relaunch = false`).
//!
//! **Never** while a box, the session survey or a wall's wait is up, on a
//! non-authoritative reading (a shell, a Codex screen outside its box), or
//! within a person's grace — a draft still changing is a person typing.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use aterm_phase::phase::{Phase, is_done_row, last_said_row, status_row};
use aterm_phase::wall::WallKind;

use super::super::config::SupervisorConfig;

/// A continuation typed at an ordinary turn end ([`decide_turn_end`]).
pub const RULE_CONTINUE: &str = "continue@v1";
/// The worker's own suggestion accepted.
pub const RULE_SUGGESTION: &str = "continue-suggestion@v1";
/// A continuation after an overload or a retryable API error's wait.
pub const RULE_API_RETRY: &str = "api-retry@v1";
/// A continuation past a usage window's reset.
pub const RULE_LIMIT_RESUME: &str = "usage-resume@v1";
/// The agent relaunched on the fallback model on a model bucket.
pub const RULE_MODEL_FALLBACK: &str = "model-fallback@v1";
/// The agent relaunched on the bucket's model again at the bucket's reset.
pub const RULE_MODEL_RESTORE: &str = "model-restore@v1";
/// `/compact` on a full context, and the continuation after it.
pub const RULE_COMPACT: &str = "context-compact@v1";
/// `/login` on a lost login.
pub const RULE_LOGIN: &str = "auth-login@v1";
/// `cfg.answer_text`, typed where a worker asked a person.
pub const RULE_ANSWER: &str = "answer@v1";
/// A model bucket's consent to go on on usage credits, accepted.
pub const RULE_CONSENT: &str = "consent-accept@v1";
/// The agent restarted in its tab for Claude Code's critical-memory banner.
pub const RULE_MEMORY_RESTART: &str = "memory-restart@v1";

/// Why the session's host restarts the agent in its tab
/// ([`TurnEndAction::Restart`]): a point nothing typed can answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restart {
    /// Claude Code's critical-memory banner (D3): nothing typed into the
    /// process is read, and no reset ends it.
    Memory,
    /// A model bucket's limit (D7): relaunched on `to`, the owner's
    /// `model_fallback` — `--model` on the relaunch line, which is
    /// session-only, where Claude's own `/model` also rewrites the person's
    /// default for every new session (2.1.282).
    Model { to: String },
    /// The bucket's reset (D7): relaunched on `to`, the bucket's model
    /// again — or, `None` (the notice named no model `/model` knows), on
    /// the model the launch itself named, else Claude's default.
    ModelBack { to: Option<String> },
}

impl Restart {
    /// The one word the journal names it by (`restart:<word>`).
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Restart::Memory => "memory",
            Restart::Model { .. } => "model",
            Restart::ModelBack { .. } => "model-back",
        }
    }
}

/// The words a worker says it is DONE with, lowercased, matched anywhere in
/// its last words ([`says_done`]): a continuation's yield that ends so is
/// short, whatever it worked (module header). What the live E2E of
/// 2026-09-25 read after its nudges (`The workflow is finished`, `nothing
/// left to do`, `what would you like me to work on?`) and their kin.
pub const DONE_PHRASES: &[&str] = &[
    "nothing left to do",
    "nothing else to do",
    "nothing more to do",
    "nothing further to do",
    "nothing left to work on",
    "no further work",
    "no remaining work",
    "no more work",
    "all done",
    "all tasks are complete",
    "all tasks complete",
    "everything is done",
    "everything is already done",
    "everything is complete",
    "already complete",
    "work is complete",
    "work is done",
    "task is complete",
    "workflow is finished",
    "workflow is complete",
    "completed successfully",
    "what would you like me to work on",
    "what would you like me to do",
    "what should i work on",
    "what else would you like",
    "let me know if you need anything",
    "let me know if there's anything else",
    // A report of finished work that asks only what comes next (the live
    // session of 2026-09-25: `Done. I've created hello.txt … What's next?`).
    "what's next?",
    "anything else?",
];

/// Whether the worker's last words say it is done ([`DONE_PHRASES`]).
#[must_use]
pub fn says_done(tail: Option<&str>) -> bool {
    tail.is_some_and(|t| {
        let flat = t
            .to_lowercase()
            .replace('’', "'")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        DONE_PHRASES.iter().any(|p| flat.contains(p))
    })
}

/// The rules whose act's yield counts toward the short streak.
fn is_continue_rule(rule: &str) -> bool {
    rule == RULE_CONTINUE || rule == RULE_SUGGESTION || rule == RULE_ANSWER
}

/// Claude Code's own `/model` aliases (2.1.280's binary: `["sonnet", "opus",
/// "haiku", "fable"]`): the only names a switch back is typed with.
pub const MODEL_ALIASES: &[&str] = &["sonnet", "opus", "haiku", "fable"];

/// The pieces a suggestion may be made of to be accepted as a continuation
/// (the owner's own accepted suggestions, measured by the audit's census:
/// `keep going`, `keep fixing forward`, `push it when green`, `keep going
/// push it when green`), joined by `,`, `;`, `.`, ` and ` or ` then `.
pub const SUGGESTION_ALLOW: &[&str] = &[
    "keep going",
    "continue",
    "keep fixing forward",
    "push it when green",
    "go on",
    "carry on",
    "keep at it",
    "proceed",
    "go ahead",
];

/// Words a worker asks a person for a decision with: the point is answered
/// ([`RULE_ANSWER`]), never continued — with [`REVERSIBLE_ANSWER`] when the
/// words name a destructive act ([`DESTRUCTIVE_WORDS`]) anywhere. Matched
/// lowercased, anywhere in the worker's last words.
pub const STOP_PHRASES: &[&str] = &[
    "need your decision",
    "needs your decision",
    "need a decision",
    "your decision on",
    "blocked on",
    "i'm blocked",
    "i am blocked",
    "waiting on you",
    "waiting for you",
    "waiting on your",
    "waiting for your",
    // "Your call" names the person's authority, not a list of options: it
    // stays a stop (the critique of 2026-09-25, O8). The choice asks are
    // [`CHOICE_PHRASES`].
    "your call",
    "need your input",
    "need your approval",
    "need you to",
    "please confirm",
    "can you confirm",
    "please advise",
    "how would you like me to proceed",
    "cannot proceed without",
    "can't proceed without",
    // Lane B2's review: the go-ahead, sign-off and review asks, and the
    // ways a worker says it will wait for one.
    "your go-ahead",
    "your go ahead",
    "the go-ahead",
    "a go-ahead",
    "sign off",
    "sign-off",
    "signoff",
    "for your review",
    "ready for review",
    "ready for your review",
    "please review",
    "your approval",
    "needs approval",
    "need approval",
    "how you'd like",
    "how you would like",
    "how would you like",
    "what you'd like me to",
    "let me know how",
    "let me know whether",
    "i'll wait",
    "i will wait",
    "i'll hold off",
    "holding off until",
    "before i proceed",
    "before i continue",
    "before i go ahead",
    "before i run",
    "before i push",
    "before i force",
    "before i delete",
    "before i merge",
    "before i deploy",
    "before i release",
];

/// Words a worker asks a person to CHOOSE with: a stop — answered, as every
/// ask is ([`RULE_ANSWER`]) — only where the phrase ASKS ([`phrase_asks`]),
/// never in a report (`documented which option each flag maps to. All tests
/// pass.`), and a stop naming the act whenever the words after it name a
/// destructive one. Matched as [`STOP_PHRASES`] are (the critique of
/// 2026-09-25).
pub const CHOICE_PHRASES: &[&str] = &[
    "which option",
    "which would you prefer",
    "which do you prefer",
    "let me know which",
];

/// Words that make an OFFER one a person must answer: an offer, a stop, a
/// choice or a question naming one of these is answered with
/// [`REVERSIBLE_ANSWER`], never continued — `keep going` would read as
/// consent (lane B2's review: `Want me to force-push this to main?`).
/// Matched as whole words in what follows the offer's phrase. A plain
/// `push` is not among them: `push it when green` is the owner's own
/// continuation (the audit's census).
pub const DESTRUCTIVE_WORDS: &[&str] = &[
    "force",
    "force-push",
    "--force",
    "delete",
    "deleting",
    "deletion",
    "drop",
    "dropping",
    "remove",
    "removing",
    "removal",
    "rm",
    "deploy",
    "deploying",
    "migrate",
    "migrating",
    "publish",
    "publishing",
    "release",
    "releasing",
    "wipe",
    "purge",
    "reset",
    "overwrite",
    "truncate",
    "destroy",
    "uninstall",
    "revert",
    "rewrite history",
];

/// Words that make a question an OFFER to go on (it counts as a
/// continuation): `want me to take that on?`.
pub const OFFER_PHRASES: &[&str] = &[
    "want me to",
    "shall i",
    "should i continue",
    "should i proceed",
    "should i keep going",
    "should i go ahead",
    // An offer of MORE work — the live probe's `Created a.txt. Should I also
    // create b.txt?` (2026-09-24) — not a plain yes/no about the work done.
    "should i also",
    "would you like me to",
    "next steps:",
    "next step:",
    "i can also",
    "if you'd like, i can",
    "let me know if you'd like",
    "happy to",
];

/// Words that recommend one option: a choice with a recommendation is the
/// worker's to take.
pub const RECOMMEND_PHRASES: &[&str] = &[
    "i recommend",
    "i'd recommend",
    "i would recommend",
    "recommended",
    "my recommendation",
    "i suggest",
    "i'd suggest",
    "i'd go with",
    "i'll go with",
    "i'd pick",
    "i lean",
];

/// The policy's timings — the defaults are the plan's; tests shorten them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndTiming {
    /// The busy work that makes a turn real work: a turn that did less is
    /// SHORT, and backs the next act off ([`Self::short_backoff`]).
    pub min_work: Duration,
    /// The wait, from the point, before the act after ONE short turn; each
    /// short turn more in a row doubles it, up to [`Self::short_backoff_max`].
    pub short_backoff: Duration,
    pub short_backoff_max: Duration,
    /// The wait before each retry of an API error or an overload, counted
    /// from the wall's appearance; the last is the wait before every retry
    /// after it.
    pub retry_backoff: Vec<Duration>,
    /// How long past a usage window's reset the continuation waits.
    pub reset_grace: Duration,
    /// How long past the time a notice that goes on BY ITSELF named
    /// (`continuing automatically at 1:50pm`) the vendor is given before the
    /// policy continues the worker itself: the vendor's own retry is waited
    /// for, never doubled — this is the net under it.
    pub auto_resume_grace: Duration,
    /// After a continuation at a usage wall that hit the wall again: the
    /// next is this far off (the last entry for every one after).
    pub limit_backoff: Vec<Duration>,
    /// The window the typing budget counts over.
    pub window: Duration,
    /// How long an act's point may show with no read of the worker busy
    /// before it is judged anyway ([`TurnEndState::observe`]): a reply that
    /// finished inside the `turn` verb's own settle, or an Enter that did
    /// not take, is never seen busy — and was a silent latch until lane B2's
    /// review.
    pub take_within: Duration,
    /// The pauses before consecutive tries of a restart the host could not
    /// make yet ([`TurnEndAction::Restart`]: not idle yet, a person's hand, a
    /// job the relaunch cannot see yet); the last for every try after it —
    /// never given up on.
    pub restart_backoff: Vec<Duration>,
}

impl Default for TurnEndTiming {
    fn default() -> Self {
        let min = |m: u64| Duration::from_secs(m * 60);
        Self {
            min_work: min(2),
            short_backoff: min(2),
            short_backoff_max: min(60),
            retry_backoff: vec![min(1), min(5), min(15), min(30), min(60)],
            reset_grace: Duration::from_secs(60),
            auto_resume_grace: min(10),
            limit_backoff: vec![min(10), min(30)],
            window: min(60),
            take_within: Duration::from_secs(30),
            restart_backoff: vec![Duration::from_secs(10), min(1), min(5), min(10)],
        }
    }
}

/// The composer as the reading found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Composer {
    /// No composer frame on the screen (a dialog, a shell).
    Absent,
    Empty,
    /// The worker's own suggestion, drawn DIM with nothing typed.
    Placeholder(String),
    /// Text is typed: a draft, never typed over — submitted as it stands
    /// once nobody has touched it for the grace ([`TurnEndAction::Submit`]).
    Typed,
}

/// What one screen says, as the turn-end policy reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndReading {
    pub phase: Phase,
    /// The reader's phase is evidence ([`aterm_phase::Reading::phase_authoritative`]).
    pub authoritative: bool,
    pub survey: bool,
    /// The wall the turn ended on.
    pub wall: Option<WallKind>,
    /// The wall's notice as said (for the model a bucket names).
    pub wall_message: String,
    /// The wall's reset as the loop's clock (the loop resolves the notice's
    /// `reset=` text with its zone and clock).
    pub reset_at: Option<Instant>,
    /// The notice says Claude Code goes on by itself (`continuing
    /// automatically at …`).
    pub resumes_by_itself: bool,
    pub composer: Composer,
    /// The worker's last words ([`aterm_phase::said_tail`]).
    pub said_tail: Option<String>,
    /// For [`TurnEndState::observe`] only: the busy work since the previous
    /// point, `None` when no read saw the worker busy since (the same point
    /// read again).
    pub worked: Option<Duration>,
    /// The standing rules, one line, when a rules file is set and readable.
    pub rules: Option<String>,
    /// The last thing on the screen is a user's `❯` row the worker has not
    /// answered yet — a continuation just submitted, before its spinner is
    /// drawn: no turn has ended here.
    pub pending_input: bool,
    /// A person stopped the turn with Esc ([`aterm_phase::interrupted`]):
    /// nothing is typed here — the next message is theirs.
    pub interrupted: bool,
    /// A host restarts the agent in its tab here
    /// ([`crate::supervise::IdleHost::can_restart`]): a restart the policy
    /// asks for can be made — the loop's to set, as [`Self::upgrading`] is.
    pub restartable: bool,
    /// The live upgrade owns the session's turn ends for now — the window's
    /// host says so from the upgrade's own state ([`crate::supervise::IdleHost::owns_turn_end`]:
    /// the agent answered READY and the restart is imminent), never from the
    /// screen: nothing is typed. A screen pattern (the last `❯` row the
    /// announcement) held a worker that answered without the marker, or an
    /// upgrade that gave up or was switched off, idle for ever, and was
    /// fooled by a 40-row read that cut the announcement off (the
    /// philosophy and hazards reviews of 2026-09-25). Never set while the
    /// loop's limit episode stands, and a wall — any wall — is decided by its
    /// own rule even where it is, with no relaunch asked for over the
    /// upgrade's own but the memory banner's ([`decide_turn_end`]).
    pub upgrading: bool,
    /// No turn yet ([`aterm_phase::Reading::fresh`]): nothing ended here.
    pub fresh: bool,
    /// How long ago a PERSON last typed or pasted into the session through
    /// its window (the server's `status human_ms=`; a control-socket write
    /// is not a person's), or the draft in the composer last changed — the
    /// later of the two, so a draft still being written holds the policy's
    /// hands on a server that does not say. `None`: neither — read as no
    /// person.
    pub person: Option<Duration>,
}

impl TurnEndReading {
    /// The reading of one screen: `aterm-phase`'s [`aterm_phase::Reading`]
    /// of `rows` by the session's program's reader (read with the cursor's
    /// column) — its composer, its interrupt — whether a draft is typed (the
    /// loop's `typed_draft`), the work since the last point, the wall's reset
    /// on the loop's clock, and the rules. What only Claude Code's screen
    /// says (a submitted `❯` row not yet answered, its `Worked for` row) is
    /// read only on Claude Code's: Codex reads a message with no end under it
    /// as busy. Whether the live upgrade owns the point is the loop's to set
    /// from its host ([`Self::upgrading`]); no screen says it.
    #[must_use]
    pub fn of(
        reading: &aterm_phase::Reading,
        rows: &[String],
        typed: bool,
        worked: Option<Duration>,
        reset_at: Option<Instant>,
        rules: Option<String>,
    ) -> Self {
        let claude = reading.program == aterm_phase::Program::Claude;
        let composer = if reading.composer.is_none() {
            Composer::Absent
        } else if typed {
            Composer::Typed
        } else if let Some(s) = &reading.suggestion {
            Composer::Placeholder(s.clone())
        } else {
            Composer::Empty
        };
        let message = reading
            .wall
            .as_ref()
            .map(|w| w.message.clone())
            .unwrap_or_default();
        Self {
            phase: reading.phase.clone(),
            authoritative: reading.phase_authoritative,
            survey: reading.survey,
            wall: reading.wall.as_ref().map(|w| w.kind),
            resumes_by_itself: super::super::limit::resumes_by_itself(&message),
            wall_message: message,
            reset_at,
            composer,
            said_tail: reading.said_tail.clone(),
            worked: worked.map(|w| {
                let done = claude.then(|| done_row_work(rows)).flatten();
                w.max(done.unwrap_or_default())
            }),
            rules,
            pending_input: claude
                && last_said_row(rows).is_some_and(|r| r.trim_start().starts_with('❯')),
            interrupted: reading.interrupted,
            restartable: false,
            upgrading: false,
            fresh: reading.fresh,
            person: None,
        }
    }

    /// A turn has ended here and nothing blocks acting on it: the phase is
    /// idle, a question or a limit — read by a reader whose word is
    /// evidence — no box or survey is up, a composer is drawn, and no input
    /// waits unanswered but one the policy judged not taken (`stale_input`,
    /// [`TurnEndState::observe`]).
    fn at_point(&self, stale_input: bool) -> bool {
        matches!(
            self.phase,
            Phase::Idle | Phase::Question | Phase::Limited { .. }
        ) && self.authoritative
            && !self.survey
            && self.composer != Composer::Absent
            && (!self.pending_input || stale_input)
    }
}

/// What a typed command leaves owed for the next point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Then {
    /// Nothing more.
    Nothing,
    /// A continuation, under the command's rule, once it has run.
    Continue,
    /// Escalated as soon as the command is typed.
    Escalate(String),
}

/// What [`decide_turn_end`] says to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEndAction {
    Nothing,
    /// Decide again at `until` (a screen change first also decides again).
    WaitUntil {
        until: Instant,
        why: String,
    },
    /// Type `text` into the composer and submit it, guarded.
    Type {
        text: String,
        rule_id: &'static str,
    },
    /// Accept the worker's own suggestion (`text`): the vendor's accept key
    /// fills it, a guarded Enter submits it.
    Accept {
        text: String,
        rule_id: &'static str,
    },
    /// Type a slash command and submit it, guarded.
    TypeCommand {
        command: String,
        rule_id: &'static str,
        then: Then,
    },
    /// Submit the draft standing in the composer, as it is, in place of the
    /// act `rule_id` names (a guarded Enter): the draft is the next message.
    Submit {
        rule_id: &'static str,
    },
    /// Hand the point to a person.
    Escalate {
        reason: String,
    },
    /// Have the session's host end the agent and relaunch it on its own
    /// conversation, then carry it on ([`Restart`]). `otherwise` is what the
    /// point gets where no restart is made (the host refused it, or none
    /// relaunches here): for the memory banner, its escalation with its
    /// remedy; for a bucket, its reset waited out; at a reset, the
    /// continuation the point was owed.
    Restart {
        why: Restart,
        rule_id: &'static str,
        otherwise: Box<TurnEndAction>,
    },
}

impl TurnEndAction {
    /// The rule an act is taken under (`None` for no act).
    #[must_use]
    pub fn rule_id(&self) -> Option<&'static str> {
        match self {
            TurnEndAction::Type { rule_id, .. }
            | TurnEndAction::Accept { rule_id, .. }
            | TurnEndAction::TypeCommand { rule_id, .. }
            | TurnEndAction::Submit { rule_id }
            | TurnEndAction::Restart { rule_id, .. } => Some(rule_id),
            _ => None,
        }
    }
}

/// Which wall a track follows (a new kind is a new track).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WallClass {
    Retry,
    Usage,
    Model,
    Context,
    Auth,
    Memory,
}

fn class_of(kind: WallKind) -> WallClass {
    match kind {
        WallKind::Overloaded | WallKind::ApiError { .. } => WallClass::Retry,
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::Spend => WallClass::Usage,
        WallKind::ModelBucket { .. } => WallClass::Model,
        WallKind::Context => WallClass::Context,
        WallKind::Auth => WallClass::Auth,
        WallKind::Memory => WallClass::Memory,
    }
}

/// A wall being worked: since when it shows (this appearance), and how many
/// acts it has had.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WallTrack {
    class: WallClass,
    since: Instant,
    attempts: usize,
}

/// A model switch this policy made (owner decision 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSwitch {
    /// The bucket's model, as a `/model` alias (`None`: the notice named
    /// none this policy knows, and nothing is switched back).
    pub from: Option<String>,
    pub to: String,
    /// The bucket's reset (`None`: it named none).
    pub back_at: Option<Instant>,
}

/// An act typed and its point not judged yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Awaited {
    rule: &'static str,
    /// A slash command, whose effect needs no turn.
    command: bool,
    /// When it was typed.
    at: Instant,
    /// When a point after it first showed with no busy read since
    /// ([`TurnEndState::observe`]).
    unseen_since: Option<Instant>,
}

impl Awaited {
    /// When its point is judged with no busy read, or it is escalated as
    /// not taken: [`TurnEndTiming::take_within`] from the first point that
    /// showed no busy read, else from the act.
    fn due(&self, timing: &TurnEndTiming) -> Instant {
        later(self.unseen_since.unwrap_or(self.at), timing.take_within)
    }
}

/// What the policy remembers of one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndState {
    pub timing: TurnEndTiming,
    /// When each typed act went, within the window.
    acts: VecDeque<Instant>,
    /// The act typed last, until the next point shows what it did.
    awaiting: Option<Awaited>,
    /// The act the current point follows (`None`: someone else's turn).
    followed: Option<&'static str>,
    /// The busy work of the turn that ended at the current point.
    point_worked: Duration,
    /// Short turns in a row ([`TurnEndTiming::min_work`]): this policy's
    /// acts' yields and anyone else's turns alike.
    short: u32,
    /// When the current point first showed: the back-off and a person's
    /// Esc grace count from it.
    point_at: Option<Instant>,
    wall: Option<WallTrack>,
    /// A command's rule whose continuation is owed at the next point.
    after: Option<&'static str>,
    model: Option<ModelSwitch>,
    /// The fallback relaunch for the bucket being worked could not be made
    /// ([`TurnEndState::restarted`]): its reset is waited out instead.
    fallback_unmade: bool,
    /// The `❯` row on the screen is an act of this policy's the worker never
    /// took (judged so at its deadline): a point, though input waits.
    stale_input: bool,
}

impl Default for TurnEndState {
    fn default() -> Self {
        Self::new(TurnEndTiming::default())
    }
}

impl TurnEndState {
    #[must_use]
    pub fn new(timing: TurnEndTiming) -> Self {
        Self {
            timing,
            acts: VecDeque::new(),
            awaiting: None,
            followed: None,
            point_worked: Duration::ZERO,
            short: 0,
            point_at: None,
            wall: None,
            after: None,
            model: None,
            fallback_unmade: false,
            stale_input: false,
        }
    }

    /// A model switch a previous loop on the session made and did not undo
    /// (the approval ledger's open `/model` row): switched back at its reset
    /// as if this loop had made it. A switch of this loop's own wins.
    pub fn seed_model(&mut self, switch: ModelSwitch) {
        if self.model.is_none() {
            self.model = Some(switch);
        }
    }

    /// Acts a previous loop on the session typed, `ages` ago (the approval
    /// ledger's `typed` rows): counted by the budget like this loop's own
    /// while they are within the window. Nothing else is restored — the
    /// short streak and a wall's attempts are read from the screen again.
    pub fn seed_acts(&mut self, ages: &[Duration], now: Instant) {
        let mut at: Vec<Instant> = ages
            .iter()
            .filter(|&&a| a < self.timing.window)
            .filter_map(|&a| now.checked_sub(a))
            .collect();
        at.extend(self.acts.drain(..));
        at.sort_unstable();
        self.acts = at.into();
        while self.acts.len() > 64 {
            self.acts.pop_front();
        }
    }

    /// The typed acts within the window ending at `now`.
    #[must_use]
    pub fn acts_in_window(&self, now: Instant) -> usize {
        self.acts
            .iter()
            .filter(|&&t| now.saturating_duration_since(t) < self.timing.window)
            .count()
    }

    /// Short turns in a row (the conformance walk's projection).
    #[must_use]
    pub fn short_streak(&self) -> u32 {
        self.short
    }

    /// The back-off before the next act after the short streak: none after
    /// real work, else [`TurnEndTiming::short_backoff`] doubled for each
    /// short turn after the first, at most [`TurnEndTiming::
    /// short_backoff_max`].
    #[must_use]
    pub fn short_wait(&self) -> Option<Duration> {
        let n = self.short.checked_sub(1)?;
        let t = &self.timing;
        Some(crate::supervise::ladder::doubling(
            t.short_backoff,
            n,
            t.short_backoff_max,
        ))
    }

    /// An act of this policy's is in flight: typed and its point not judged
    /// yet, or a command's continuation owed.
    #[must_use]
    pub fn act_in_flight(&self) -> bool {
        self.awaiting.is_some() || self.after.is_some()
    }

    /// The model switch in force, if any.
    #[must_use]
    pub fn model_switch(&self) -> Option<&ModelSwitch> {
        self.model.as_ref()
    }

    /// Fold what a point shows into the state, BEFORE [`decide_turn_end`] is
    /// asked about it: the yield of the act typed last (a continuation or an
    /// answer that produced less than `min_work` lengthens the short streak,
    /// one that produced more ends it), someone else's turn in between
    /// (`worked` with nothing awaited: short, or the streak's end), the
    /// first point this state sees (its work unknown: short), and the wall on
    /// the screen (a new kind opens a track; the same kind again
    /// after a retry is its next appearance). Reading the same point again
    /// (`worked` `None`, nothing awaited) changes nothing — and neither does
    /// a point after a TEXT act with no busy read since: the worker has not
    /// taken it yet (Claude Code draws a submitted message under the last
    /// done row, where it reads as queued, before its spinner comes), so
    /// that point is not the act's and the act stays awaited — for
    /// [`TurnEndTiming::take_within`] from the first such point. Past that,
    /// the point IS the act's, with no work seen: a reply that finished
    /// inside the `turn` verb's settle (`Nothing left to do.`), a wall that
    /// answered at once, an Enter that did not take — each judged as the
    /// short yield it is, never a latch. A slash command's point needs no
    /// busy read (`/model` answers at once). An act whose `❯` row stays last
    /// and unanswered, no busy read since, is waited for the same way — and
    /// past the deadline judged NOT TAKEN: the same short yield, its row
    /// then a point the next act may go at ([`decide_turn_end`]), on the
    /// back-off. Anyone else's unanswered row is never a point.
    pub fn observe(&mut self, reading: &TurnEndReading, now: Instant) {
        if matches!(reading.phase, Phase::Busy | Phase::Prompt) {
            return;
        }
        if !reading.pending_input || reading.worked.is_some() {
            self.stale_input = false;
        }
        if reading.pending_input {
            let Some(a) = self.awaiting.as_mut().filter(|_| reading.worked.is_none()) else {
                return;
            };
            let since = *a.unseen_since.get_or_insert(now);
            if now < later(since, self.timing.take_within) {
                return;
            }
            self.stale_input = true;
        } else if let Some(a) = &mut self.awaiting
            && !a.command
            && reading.worked.is_none()
        {
            let since = *a.unseen_since.get_or_insert(now);
            if now < later(since, self.timing.take_within) {
                return;
            }
        }
        let min = self.timing.min_work;
        let awaited = self.awaiting.take().map(|a| a.rule);
        let done = says_done(reading.said_tail.as_deref());
        let streak = |short: u32, d: Duration, done: bool| {
            if d < min || done {
                short.saturating_add(1)
            } else {
                0
            }
        };
        match (awaited, reading.worked) {
            (Some(rule), worked) => {
                let d = worked.unwrap_or_default();
                self.followed = Some(rule);
                self.point_worked = d;
                self.point_at = Some(now);
                if is_continue_rule(rule) {
                    self.short = streak(self.short, d, done);
                }
            }
            // A turn that ended on a wall is not the worker done: the wall's
            // own ladder waits it out, and the streak is left as it was.
            (None, Some(d)) => {
                self.followed = None;
                self.point_worked = d;
                self.point_at = Some(now);
                if reading.wall.is_none() {
                    self.short = streak(self.short, d, false);
                }
            }
            // The first point seen, its work unknown: a supervisor that
            // attaches to an idle worker backs off before it types. A
            // session with no turn yet ended nothing: no streak begins.
            (None, None) if self.point_at.is_none() && !reading.fresh => {
                self.point_at = Some(now);
                self.short = 1;
            }
            (None, None) => {}
        }
        let retried = awaited.is_some_and(|r| r == RULE_API_RETRY || r == RULE_LIMIT_RESUME);
        match reading.wall.map(class_of) {
            Some(class) => match &mut self.wall {
                Some(t) if t.class == class => {
                    if retried {
                        t.since = now;
                    }
                }
                _ => {
                    self.wall = Some(WallTrack {
                        class,
                        since: now,
                        attempts: 0,
                    });
                    self.fallback_unmade = false;
                }
            },
            // The wall has left the screen: gone for good once the worker
            // worked (or our retry was taken). With no work — a human's
            // `/login` or `/model` output over it — a usage track stays, and
            // the continuation goes at once ([`decide_turn_end`]).
            None => {
                if reading.worked.is_some() || retried {
                    self.wall = None;
                    self.fallback_unmade = false;
                }
            }
        }
    }

    /// Record an act the loop TYPED (a skipped or refused one is not): the
    /// budget, the act awaited, what it leaves owed, and — for a wall's act
    /// — the track's attempts. A restart is no typed act
    /// ([`Self::restarted`]).
    pub fn acted(&mut self, action: &TurnEndAction, reading: &TurnEndReading, now: Instant) {
        let _ = reading;
        if matches!(action, TurnEndAction::Restart { .. }) {
            return;
        }
        let Some(rule) = action.rule_id() else {
            return;
        };
        self.acts.push_back(now);
        while self.acts.len() > 64 {
            self.acts.pop_front();
        }
        self.awaiting = Some(Awaited {
            rule,
            command: matches!(action, TurnEndAction::TypeCommand { .. }),
            at: now,
            unseen_since: None,
        });
        self.after = match action {
            TurnEndAction::TypeCommand {
                then: Then::Continue,
                ..
            } => Some(rule),
            _ => None,
        };
        if matches!(
            rule,
            RULE_API_RETRY | RULE_LIMIT_RESUME | RULE_CONSENT | RULE_COMPACT | RULE_LOGIN
        ) && let Some(t) = &mut self.wall
        {
            t.attempts += 1;
        }
    }
}

impl TurnEndState {
    /// Record a restart the loop asked its host for
    /// ([`TurnEndAction::Restart`]): MADE (`made`, the new process the
    /// loop's) — a bucket's fallback is the switch to undo at the reset
    /// ([`Self::model_switch`], back at the reset the notice named, else
    /// after the longest limit back-off), the reset's relaunch undoes it,
    /// and the bucket's track counts the act; or NOT made (the host refused
    /// it, or none relaunches here) — a bucket's reset is waited out
    /// instead, and a switch that cannot be undone is let go (it was
    /// session-only: nothing else moved). The memory banner's leaves nothing
    /// to remember.
    pub fn restarted(&mut self, why: &Restart, made: bool, reading: &TurnEndReading, now: Instant) {
        match why {
            Restart::Memory => {}
            Restart::Model { to } if made => {
                let back_at = reading.reset_at.or_else(|| {
                    self.timing
                        .limit_backoff
                        .last()
                        .and_then(|&b| now.checked_add(b))
                });
                self.model = Some(ModelSwitch {
                    from: bucket_model(&reading.wall_message),
                    to: to.clone(),
                    back_at,
                });
                if let Some(t) = &mut self.wall {
                    t.attempts += 1;
                }
            }
            Restart::Model { .. } => self.fallback_unmade = true,
            Restart::ModelBack { .. } => self.model = None,
        }
    }
}

/// The `/model` alias a bucket's notice names (`You've reached your Fable
/// limit` → `fable`, `Opus 4.1 limit reached` → `opus`): the first word that
/// is one of [`MODEL_ALIASES`].
#[must_use]
pub fn bucket_model(message: &str) -> Option<String> {
    message
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .find(|w| MODEL_ALIASES.contains(&w.as_str()))
}

/// The work a finished turn's done row reports (`✻ Cooked for 3m 2s · done
/// 4:24 PM` → 182 s): Claude Code's own measure of the turn, which a
/// supervisor that started mid-turn saw only part of.
#[must_use]
pub fn done_row_work(rows: &[String]) -> Option<Duration> {
    let row = status_row(rows).filter(|r| is_done_row(r))?;
    let (_, rest) = row.split_once(" for ")?;
    let span = rest.split(" · ").next()?.trim();
    let mut secs = 0u64;
    for word in span.split_whitespace() {
        let unit = word.chars().last()?;
        let n: u64 = word[..word.len() - unit.len_utf8()].parse().ok()?;
        secs += n * match unit {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
    }
    Some(Duration::from_secs(secs))
}

/// Whether a suggestion is a continuation: made only of
/// [`SUGGESTION_ALLOW`] pieces, case and closing punctuation aside.
#[must_use]
pub fn suggestion_allowed(text: &str) -> bool {
    let lower = text
        .to_lowercase()
        .replace(" and ", ",")
        .replace(" then ", ",");
    let mut any = false;
    for piece in lower.split([',', ';', '.', '!']) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        if !SUGGESTION_ALLOW.contains(&piece) {
            return false;
        }
        any = true;
    }
    any
}

/// What the worker's last words ask of a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// A report, or nothing: the point may be continued.
    Plain,
    /// An offer to go on (`want me to take that on?`): continued.
    Offer,
    /// A question for a person: answered.
    Question,
    /// A decision asked for: answered, with why.
    Stop(String),
    /// A decision that names an irreversible act ([`destructive_word`]) —
    /// an offer to do it, a question or a choice that names it anywhere:
    /// answered only with [`REVERSIBLE_ANSWER`], with why.
    Irreversible(String),
}

/// The worker's last words, judged ([`Said`]). `tail` comes one line per
/// SCREEN ROW ([`aterm_phase::said_tail`]), so a soft wrap is a line break
/// that means nothing: phrases are matched on the rows joined with single
/// spaces, and the question is the last SENTENCE of the last unit — the
/// rows from the last enumerated option row on, joined — never the last
/// row (lane B2's review: `Want me to keep the old API⏎or drop it?` and a
/// stop phrase split by a wrap were continued). A stop phrase anywhere
/// wins — naming the act when a destructive word is anywhere in the tail
/// (`I'll wait for your go-ahead before force-pushing.`); then an offer to do something destructive ([`DESTRUCTIVE_WORDS`]
/// anywhere after the FIRST offer phrase — a benign offer after a
/// destructive one does not hide it) is a stop, and so is any question
/// that names a destructive act, offer or not (`Delete the stale branches
/// now? (y/n)`); then a last sentence that asks (`?`, or a trailing `(y/n)`
/// / `[y/N]` / `(yes/no)`, a trailing parenthetical aside after it not
/// counting): a choice between options with no recommendation — two or more
/// listed options under an ask that CHOOSES ([`asks_to_choose`]: `Which
/// one?`, `Shall I start?`, never `Did the suite pass?`), or an either/or the
/// worker would act on ([`either_or`]: `should I … or …`, never `Did it pass
/// on your machine, or should I rerun it?`) — is a stop, and so is a
/// [`CHOICE_PHRASES`] ask where it asks ([`phrase_asks`]); ANY destructive
/// word anywhere in the tail of a choice makes it a stop that names the act
/// (the adversarial review of 2026-09-25: `1. Clean slate⏎Delete build/ and
/// target/⏎2. Incremental⏎Which do you prefer?` — the act was on a
/// description row), inflections included ([`destructive_word`]); an offer
/// is an offer, anything else a question; a tail that does not ask is an
/// offer when it opens next steps, else plain. (The safety review of
/// 2026-09-24: each of those shapes was continued, and `keep going` reads as
/// yes.) Every stop and question is ANSWERED ([`RULE_ANSWER`]) — one that
/// names an irreversible act ([`Said::Irreversible`]) only with
/// [`REVERSIBLE_ANSWER`]; the reason is what an escalation says where
/// `answer_questions = false`.
#[must_use]
pub fn classify_said(tail: Option<&str>) -> Said {
    let Some(tail) = tail else {
        return Said::Plain;
    };
    let lower = yes_no_asks(&tail.to_lowercase().replace('’', "'"));
    let flat = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(p) = STOP_PHRASES.iter().find(|p| flat.contains(*p)) {
        // A stop that names a destructive act anywhere asks consent to it
        // (`I'll wait for your go-ahead before force-pushing.`): whatever
        // answers it must consent to nothing irreversible, as a choice's.
        if let Some(w) = destructive_word(&flat) {
            return Said::Irreversible(format!("the worker said \"{p}\" of an act that would {w}"));
        }
        return Said::Stop(format!("the worker said \"{p}\""));
    }
    let recommends = RECOMMEND_PHRASES.iter().any(|p| flat.contains(p));
    let offer_at = OFFER_PHRASES.iter().filter_map(|p| flat.find(p)).min();
    if let Some(at) = offer_at
        && let Some(w) = destructive_word(&flat[at..])
    {
        return Said::Irreversible(format!("the worker offers to {w}"));
    }
    if let Some(w) = questions(&flat).into_iter().find_map(destructive_word) {
        return Said::Irreversible(format!("the worker asks whether to {w}"));
    }
    let offer = offer_at.is_some();
    let lines: Vec<&str> = lower
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let unit_start = lines.iter().rposition(|l| is_option_line(l)).unwrap_or(0);
    let unit = lines[unit_start..].join(" ");
    let unit = without_trailing_aside(&unit).trim_end_matches([')', '"', '*', '`', ' ']);
    let asks = unit.ends_with('?');
    let listed = lines.iter().filter(|l| is_option_line(l)).count();
    let ask = last_sentence(unit);
    // A choice between options: two or more listed and a choosing ask, or an
    // either/or the worker would act on.
    let listed_choice = listed >= 2 && asks_to_choose(ask);
    let chooses = asks && (listed_choice || either_or(ask));
    let choice_phrase = CHOICE_PHRASES
        .iter()
        .filter_map(|p| flat.find(p).map(|at| (*p, at)))
        .min_by_key(|&(_, at)| at);
    let phrase_choice = choice_phrase.filter(|&(p, at)| phrase_asks(&flat, &lines, p, at));
    // A choice that names a destructive act ANYWHERE in the tail — an
    // option, its description row, the ask, the report before it: whatever
    // answers it consents to that act.
    if (chooses || phrase_choice.is_some())
        && let Some(w) = destructive_word(&flat)
    {
        return Said::Irreversible(format!("the worker offers a choice that would {w}"));
    }
    if chooses && !recommends {
        return Said::Stop(if listed_choice {
            "the worker listed options with no recommendation".to_string()
        } else {
            "the worker asks to choose between options".to_string()
        });
    }
    if let Some((p, _)) = phrase_choice {
        return Said::Stop(format!("the worker said \"{p}\""));
    }
    // A choice phrase that does not ask (a report that says `which option`)
    // is no choice — but an act named after it is still a person's.
    if let Some((_, at)) = choice_phrase
        && let Some(w) = destructive_word(&flat[at..])
    {
        return Said::Irreversible(format!("the worker asks to choose whether to {w}"));
    }
    if !asks {
        return if offer { Said::Offer } else { Said::Plain };
    }
    if offer { Said::Offer } else { Said::Question }
}

/// The yes/no markers a worker closes a question with, lowercased.
const YES_NO_MARKERS: &[&str] = &["(y/n)", "[y/n]", "(yes/no)", "[yes/no]", "(y/n)?", "[y/n]?"];

/// `text` (lowercased) with each yes/no marker ([`YES_NO_MARKERS`]) read as
/// the `?` it means: `delete them now? (y/n)` and `proceed (y/n)` both ask.
fn yes_no_asks(text: &str) -> String {
    let mut out = text.to_string();
    for m in YES_NO_MARKERS {
        out = out.replace(m, "?");
    }
    out
}

/// Every sentence of `flat` that asks — ends in a `?` followed by a space or
/// the end — whole, from the last sentence end before it.
fn questions(flat: &str) -> Vec<&str> {
    let bytes = flat.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in bytes.iter().enumerate() {
        let ends = matches!(bytes.get(i + 1), None | Some(b' '));
        if !ends {
            continue;
        }
        match b {
            b'?' => {
                out.push(flat[start..=i].trim());
                start = i + 1;
            }
            b'.' | b'!' | b';' | b':' => start = i + 1,
            _ => {}
        }
    }
    out
}

/// `unit` without a parenthetical aside after its last sentence
/// (`Should I rename it? (I have not touched it yet.)` asks).
fn without_trailing_aside(unit: &str) -> &str {
    let mut t = unit.trim_end();
    while t.ends_with(')') {
        let Some(open) = t.rfind('(') else {
            break;
        };
        let before = t[..open].trim_end();
        if !before.ends_with(['?', '.', '!']) {
            break;
        }
        t = before;
    }
    t
}

/// The last sentence of `text`: what follows the last `.`, `!`, `?`, `:` or
/// `;` that is followed by a space (so `v1.2` and `a.rs` do not end one).
fn last_sentence(text: &str) -> &str {
    let body = text.trim_end_matches('?');
    let mut start = 0;
    let bytes = body.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?' | b':' | b';') && bytes.get(i + 1) == Some(&b' ') {
            start = i + 1;
        }
    }
    text[start..].trim()
}

/// What, found before an either/or's ` or `, makes it one the worker would
/// act on: `should I rewrite the parser or patch the lexer?`, `want me to
/// keep the old API or rename it?`, `which would you prefer: … or …?`. A
/// question with none of them — `did the suite pass on your machine, or
/// should I rerun it?` — asks the person something only they know.
pub const CHOICE_LEADS: &[&str] = &[
    "should i",
    "shall i",
    "should we",
    "shall we",
    "want me to",
    "do you want",
    "would you like",
    "prefer",
    "which",
];

/// Whether the asking sentence `ask` (lowercased, its `?` trimmed) is an
/// either/or the worker would act on ([`CHOICE_LEADS`] before its ` or `).
fn either_or(ask: &str) -> bool {
    ask.find(" or ")
        .is_some_and(|at| CHOICE_LEADS.iter().any(|l| ask[..at].contains(l)))
}

/// Whether the asking sentence `ask` under a list of options asks to CHOOSE
/// one — `which one?`, `which do you prefer?`, `shall I start?`, `1 or
/// 2?` — rather than something else about the work (`did the suite pass on
/// your machine?`, `does that look right?`: a question).
fn asks_to_choose(ask: &str) -> bool {
    let words: Vec<&str> = ask
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .collect();
    either_or(ask)
        || CHOICE_LEADS.iter().any(|l| ask.contains(l))
        || words.iter().any(|w| {
            matches!(
                *w,
                "which" | "option" | "options" | "choose" | "pick" | "either" | "or"
            )
        })
}

/// Whether a [`CHOICE_PHRASES`] ask `p`, found at `at` in `flat`, ASKS: the
/// sentence it is in ends `?` (`which would you prefer: the arena or the
/// slab?`), or it is in the tail's last sentence (`let me know which layout
/// you want.`), or its row introduces the enumerated options (`let me know
/// which you prefer:⏎- rebase⏎- open a new PR`). A phrase in an earlier
/// sentence of a report (`documented which option each flag maps to. All
/// tests pass.`) does not.
fn phrase_asks(flat: &str, lines: &[&str], p: &str, at: usize) -> bool {
    let rest = &flat[at..];
    let bytes = rest.as_bytes();
    // The first sentence end after the phrase: `.`, `!` or `?` followed by
    // a space or the end — a `.` after a digit (`1. drop`) is an option's
    // number, not one.
    let end = bytes.iter().enumerate().position(|(i, b)| {
        matches!(b, b'.' | b'!' | b'?')
            && matches!(bytes.get(i + 1), None | Some(b' '))
            && !(*b == b'.' && i > 0 && bytes[i - 1].is_ascii_digit())
    });
    let asks_here = end.is_none_or(|i| bytes[i] == b'?' || i + 1 == bytes.len());
    let introduces = lines
        .iter()
        .position(|l| l.contains(p))
        .is_some_and(|k| lines.get(k + 1).is_some_and(|l| is_option_line(l)));
    asks_here || introduces
}

/// The first of [`DESTRUCTIVE_WORDS`] in `text`, as a whole word (a phrase
/// as whole words) or an inflection of one ([`inflects`]: `deletes`,
/// `dropped`, `wiping`, `force-pushed` — the adversarial review of
/// 2026-09-25: `1. Fresh start (removes node_modules)` matched no base form
/// and was answered), named by its base form.
pub(crate) fn destructive_word(text: &str) -> Option<&'static str> {
    let words: Vec<&str> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .map(|w| w.trim_start_matches('-'))
        .collect();
    DESTRUCTIVE_WORDS.iter().copied().find(|d| {
        let want: Vec<&str> = d.split(' ').map(|w| w.trim_start_matches('-')).collect();
        words.windows(want.len()).any(|win| {
            win.iter()
                .zip(want.iter())
                .all(|(w, base)| inflects(w, base))
        })
    })
}

/// Whether `word` is `base` or one of its regular inflections: `-s`/`-es`,
/// `-d`/`-ed` and `-ing` — a final `e` dropped before them (`deleting`,
/// `removed`), a final consonant doubled (`dropped`, `resetting`) — plus
/// `-en` after a doubled `t` (`overwritten`) and the one irregular past the
/// list needs (`overwrote`, `rewrote`). Over-matching a noun (`drops`,
/// `releases`) only stops a point that would have been answered; missing a
/// verb answers one a person had to.
fn inflects(word: &str, base: &str) -> bool {
    if word == base {
        return true;
    }
    let Some(rest) = word.strip_prefix(base) else {
        // A final `e` dropped: `delet` + `ing`/`ed`; `overwrit` + `ten`.
        let stem = base.strip_suffix('e').unwrap_or(base);
        return (stem.len() < base.len()
            && word
                .strip_prefix(stem)
                .is_some_and(|rest| matches!(rest, "ing" | "ed" | "ten")))
            || (base.ends_with("write") && word == format!("{}ote", &base[..base.len() - 3]));
    };
    if matches!(rest, "s" | "es" | "d" | "ed" | "ing" | "ten") {
        return true;
    }
    // A final consonant doubled: `drop` + `ped`, `reset` + `ting`.
    let last = base.chars().last().unwrap_or(' ');
    rest.strip_prefix(last)
        .is_some_and(|rest| matches!(rest, "ed" | "ing" | "en"))
}

/// An enumerated option row: `1.`, `2)`, `(a)`, `a)`, `- `, `• `,
/// `option a`/`option 1`.
fn is_option_line(line: &str) -> bool {
    let t = line.trim_start();
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && t[digits..].starts_with(['.', ')']) {
        return true;
    }
    let mut cs = t.chars();
    if let (Some(a), Some(b)) = (cs.next(), cs.next())
        && a.is_ascii_lowercase()
        && b == ')'
    {
        return true;
    }
    t.starts_with("(a)")
        || t.starts_with("(b)")
        || t.starts_with("- ")
        || t.starts_with("• ")
        || t.starts_with("option ")
}

/// What to type as a continuation: `cfg.continue_text`, with the standing
/// rules when there are any.
#[must_use]
pub fn continuation_text(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    with_rules(&cfg.continue_text, rules)
}

/// What to type as an answer to a question for a person: `cfg.answer_text`,
/// with the standing rules when there are any.
#[must_use]
pub fn answer_text(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    with_rules(&cfg.answer_text, rules)
}

/// What a question or a choice that names an irreversible act is answered
/// with ([`Said::Irreversible`], D1 of the reconciliation of 2026-09-25):
/// never "take the option you would recommend" — the recommendation may be
/// the act — and not the owner's `answer_text`, which may say so.
pub const REVERSIBLE_ANSWER: &str = "Nobody is here to approve an irreversible step: take the \
                                     option that deletes, overwrites and force-pushes nothing, \
                                     and keep going.";

/// [`REVERSIBLE_ANSWER`], with the standing rules when there are any.
#[must_use]
pub fn reversible_answer(rules: Option<&str>) -> String {
    with_rules(REVERSIBLE_ANSWER, rules)
}

fn with_rules(text: &str, rules: Option<&str>) -> String {
    match rules.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => format!("{text} (standing rules: {r})"),
        None => text.to_string(),
    }
}

/// `at + d`, saturating at `at` when the sum overflows the clock.
fn later(at: Instant, d: Duration) -> Instant {
    at.checked_add(d).unwrap_or(at)
}

/// THE DECISION at one point (module header). Pure: the same state, reading,
/// policy and clock give the same action.
#[must_use]
pub fn decide_turn_end(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> TurnEndAction {
    // A point the live upgrade owns (its host's word) is the upgrade's —
    // except at a WALL: the upgrade types nothing there and its host takes
    // no step at one (the loop's `host_steps_here`), so EVERY wall is decided
    // by its own rule — a limit waited out and continued past its reset, an
    // overloaded or failed API call retried, a full context compacted, a lost
    // login typed — with no relaunch asked for over the upgrade's own (a
    // model bucket is waited out, never relaunched on its fallback). The
    // reviews of 2026-09-26: a wind-down turn that hit a limit naming its
    // reset, `resets 7:30pm`, was held here for good; and so, after that fix,
    // was one that ended on `529 Overloaded` or a full context — nobody
    // typing its retry and the host never stepping. The memory banner keeps
    // its restart: its one remedy is one, and the upgrade's own needs a
    // READY an agent past reading never gives. And the continuation a wall's
    // act OWES (`/compact`'s `then: Continue`) is paid at the idle point after
    // it, owned or not: an owed act is an act in flight, which keeps the
    // host out too, so muted here it was paid by nobody and the agent sat
    // compacted for good (review of 2026-09-27).
    if reading.upgrading {
        match reading.wall {
            None if state.after.is_none() => return TurnEndAction::Nothing,
            None => {}
            Some(WallKind::Memory) => {}
            Some(_) if reading.restartable => {
                let unrestartable = TurnEndReading {
                    restartable: false,
                    ..reading.clone()
                };
                return decide_turn_end(state, &unrestartable, cfg, now);
            }
            Some(_) => {}
        }
    }
    // Claude Code's critical-memory banner: nothing is typed at it, so what
    // holds a TYPED act back — a draft, an unanswered message, the survey,
    // an act awaited, a spinner — does not hold its answer back (the host's
    // restart takes its own last looks: idle, nobody's hand). A turn a
    // person stopped is theirs for the grace below, banner or not.
    if reading.wall == Some(WallKind::Memory) && !reading.interrupted {
        return memory_restart(cfg, reading);
    }
    // An act typed and its point not judged yet ([`TurnEndState::observe`],
    // which judges it at its deadline — taken or not): decided again then.
    if let Some(a) = &state.awaiting {
        if !reading.authoritative
            || reading.survey
            || matches!(reading.phase, Phase::Busy | Phase::Prompt)
        {
            return TurnEndAction::Nothing;
        }
        let due = a.due(&state.timing);
        if now < due {
            return TurnEndAction::WaitUntil {
                until: due,
                why: format!("the worker to take the {} act", a.rule),
            };
        }
    }
    if !reading.at_point(state.stale_input) || reading.fresh {
        return TurnEndAction::Nothing;
    }
    // A person at the keyboard wins: hands off for the grace after their last
    // keystroke, and after a turn they stopped with Esc (itself a keystroke,
    // whether or not the server says so) from the point on.
    let grace = Duration::from_secs(u64::from(cfg.human_grace_s));
    let held = match (reading.person, reading.interrupted) {
        (Some(ago), _) if ago < grace => Some((later(now, grace - ago), "a person is typing")),
        (_, true) => Some((
            later(state.point_at.unwrap_or(now), grace),
            "a person stopped the turn with Esc",
        )),
        _ => None,
    };
    if let Some((until, why)) = held.filter(|(until, _)| now < *until) {
        return TurnEndAction::WaitUntil {
            until,
            why: why.to_string(),
        };
    }
    let act = act_at_point(state, reading, cfg, now);
    // A draft standing in the composer (the grace above has passed since it
    // last changed): submitted in the act's place, under its rule — never
    // typed over.
    match act.rule_id() {
        Some(rule_id) if reading.composer == Composer::Typed => TurnEndAction::Submit { rule_id },
        _ => act,
    }
}

/// The act at a free point ([`decide_turn_end`], past a person's grace):
/// a wall's, an owed continuation, a switch back, or the continue policy's.
fn act_at_point(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> TurnEndAction {
    // `continue_per_hour = 0` is no cap.
    let budgeted = |act: TurnEndAction| {
        let used = state.acts_in_window(now);
        if cfg.continue_per_hour != 0 && used >= cfg.continue_per_hour as usize {
            TurnEndAction::Escalate {
                reason: format!(
                    "typing budget spent: {used} acts in the last {} min",
                    state.timing.window.as_secs() / 60
                ),
            }
        } else {
            act
        }
    };
    let continue_as = |rule: &'static str| {
        budgeted(TurnEndAction::Type {
            text: continuation_text(cfg, reading.rules.as_deref()),
            rule_id: rule,
        })
    };
    if let Some(kind) = reading.wall {
        return wall_action(state, reading, cfg, now, kind, &budgeted, &continue_as);
    }
    // A command's continuation, owed once it has run.
    if let Some(rule) = state.after {
        return continue_as(rule);
    }
    // The screen left a limit it was waiting out, with no work and no act
    // of ours (a human's `/login`, `/model`): the continuation goes at once.
    if let Some(t) = &state.wall
        && (t.class == WallClass::Usage || t.class == WallClass::Model)
        && t.attempts == 0
        && cfg.resume_limits
    {
        return continue_as(RULE_LIMIT_RESUME);
    }
    let cont = continue_policy(state, reading, cfg, now, &budgeted);
    // The bucket's reset has come: relaunched on the bucket's model first,
    // which carries it on — or, where no relaunch is made, the point's own
    // act, the model left as it is (session-only: nothing else moved).
    if let Some(m) = &state.model
        && let Some(at) = m.back_at
        && now >= at
        && reading.restartable
        && !matches!(cont, TurnEndAction::Escalate { .. })
    {
        return TurnEndAction::Restart {
            why: Restart::ModelBack { to: m.from.clone() },
            rule_id: RULE_MODEL_RESTORE,
            otherwise: Box::new(cont),
        };
    }
    cont
}

/// The continue policy at a point with no wall: the question answered or
/// the turn continued (the worker's own suggestion first) once the short
/// streak's back-off has passed; a question escalated only where the owner
/// switched the answers off.
fn continue_policy(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> TurnEndAction {
    // What the worker asks of a person — nothing, after a person's Esc: the
    // point is theirs, and resuming it is a continuation. A question the
    // reader could not read the words of is never taken for "nothing asked".
    let mut irreversible = false;
    let asked = if reading.interrupted {
        None
    } else if reading.phase == Phase::Question && reading.said_tail.is_none() {
        Some("the worker asked something its last words do not show".to_string())
    } else {
        match classify_said(reading.said_tail.as_deref()) {
            Said::Irreversible(why) => {
                irreversible = true;
                Some(why)
            }
            Said::Stop(why) => Some(why),
            Said::Question => Some("the worker asked a question".to_string()),
            Said::Offer | Said::Plain => None,
        }
    };
    match &asked {
        Some(why) if !cfg.answer_questions => {
            return TurnEndAction::Escalate {
                reason: format!("{why}; answer_questions is off"),
            };
        }
        None if !cfg.continue_policy => return TurnEndAction::Nothing,
        _ => {}
    }
    if let (Some(wait), Some(at)) = (state.short_wait(), state.point_at) {
        let due = later(at, wait);
        if now < due {
            let secs = wait.as_secs();
            return TurnEndAction::WaitUntil {
                until: due,
                why: if secs < 120 {
                    format!("{} short turn(s) in a row: {secs} s back-off", state.short)
                } else {
                    format!(
                        "{} short turn(s) in a row: {} min back-off",
                        state.short,
                        secs / 60
                    )
                },
            };
        }
    }
    if asked.is_some() {
        let rules = reading.rules.as_deref();
        return budgeted(TurnEndAction::Type {
            text: if irreversible {
                reversible_answer(rules)
            } else {
                answer_text(cfg, rules)
            },
            rule_id: RULE_ANSWER,
        });
    }
    match &reading.composer {
        Composer::Placeholder(s) if suggestion_allowed(s) => budgeted(TurnEndAction::Accept {
            text: s.clone(),
            rule_id: RULE_SUGGESTION,
        }),
        _ => budgeted(TurnEndAction::Type {
            text: continuation_text(cfg, reading.rules.as_deref()),
            rule_id: RULE_CONTINUE,
        }),
    }
}

/// A limit waited out: the continuation [`TurnEndTiming::reset_grace`] past
/// the reset the notice names — or, after a continuation that hit the wall
/// again, [`TurnEndTiming::limit_backoff`] from that appearance, whichever is
/// later; with no reset read, the longest backoff from the appearance.
fn resume_at_reset(
    state: &TurnEndState,
    reading: &TurnEndReading,
    now: Instant,
    since: Instant,
    attempts: usize,
    kind: WallKind,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
) -> TurnEndAction {
    let back = &state.timing.limit_backoff;
    let backoff = |i: usize| crate::supervise::ladder::Ladder(back).at(i);
    let at_reset = reading.reset_at.map(|r| later(r, state.timing.reset_grace));
    let after_retry = if attempts == 0 {
        None
    } else {
        backoff(attempts - 1).map(|b| later(since, b))
    };
    let due = match (at_reset, after_retry) {
        (Some(r), Some(b)) => r.max(b),
        (Some(r), None) => r,
        (None, Some(b)) => b,
        // No reset the loop could read: the longest backoff.
        (None, None) => match back.last() {
            Some(&b) => later(since, b),
            None => return TurnEndAction::Nothing,
        },
    };
    if now < due {
        return TurnEndAction::WaitUntil {
            until: due,
            why: format!("{} resets", kind.name()),
        };
    }
    continue_as(RULE_LIMIT_RESUME)
}

/// The retry ladder's wait before the attempt after `attempts`: its entry,
/// the last for every attempt past it.
fn retry_wait(state: &TurnEndState, attempts: usize) -> Option<Duration> {
    crate::supervise::ladder::Ladder(&state.timing.retry_backoff).at(attempts)
}

/// The wall policies (module header).
fn wall_action(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    kind: WallKind,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
) -> TurnEndAction {
    let track = state.wall.as_ref().filter(|t| t.class == class_of(kind));
    let since = track.map_or(now, |t| t.since);
    let attempts = track.map_or(0, |t| t.attempts);
    let esc = |reason: String| TurnEndAction::Escalate { reason };
    let msg = &reading.wall_message;
    let wait_out = || {
        if cfg.resume_limits {
            resume_at_reset(state, reading, now, since, attempts, kind, continue_as)
        } else {
            esc(format!("{}: {msg} (resume_limits is off)", kind.name()))
        }
    };
    match kind {
        WallKind::Overloaded | WallKind::ApiError { .. } => {
            if !cfg.retry_api_errors {
                return esc(format!("{}: {msg} (retry_api_errors is off)", kind.name()));
            }
            let Some(wait) = retry_wait(state, attempts) else {
                return continue_as(RULE_API_RETRY);
            };
            let due = later(since, wait);
            if now < due {
                return TurnEndAction::WaitUntil {
                    until: due,
                    why: format!("{} retry {}", kind.name(), attempts + 1),
                };
            }
            continue_as(RULE_API_RETRY)
        }
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::Spend => {
            if reading.resumes_by_itself && cfg.resume_limits {
                // The vendor goes on by itself: waited out — handled, nobody
                // is needed (the philosophy review of 2026-09-25: `Nothing`
                // here opened an escalated episode, a badge and a mail for a
                // wall that needed no one) — and continued only if it has
                // not gone on [`TurnEndTiming::auto_resume_grace`] past the
                // time it named (with none, the longest limit back-off).
                let t = &state.timing;
                let due = match reading.reset_at {
                    Some(at) => later(at, t.auto_resume_grace),
                    None => later(since, t.limit_backoff.last().copied().unwrap_or_default()),
                };
                if now < due {
                    return TurnEndAction::WaitUntil {
                        until: due,
                        why: format!("{} goes on by itself", kind.name()),
                    };
                }
                return continue_as(RULE_LIMIT_RESUME);
            }
            if reading.resumes_by_itself {
                return TurnEndAction::Nothing;
            }
            if !cfg.resume_limits && kind != WallKind::Spend {
                // The limit episode tells the person (the loop's
                // `limit_after`).
                return TurnEndAction::Nothing;
            }
            wait_out()
        }
        // A bucket that asks consent to go on on usage credits: accepted
        // (module header) — continued, where the vendor's confirm is a box
        // the approval policy answers. Back after that, or with the
        // continuation limited: switched off, or waited out, like any bucket.
        WallKind::ModelBucket { consent } => {
            if consent && attempts == 0 && cfg.continue_policy {
                return continue_as(RULE_CONSENT);
            }
            let Some(fallback) = &cfg.model_fallback else {
                return wait_out();
            };
            if state.model.is_some()
                || state.fallback_unmade
                || !reading.restartable
                || bucket_model(msg).as_deref() == Some(fallback.as_str())
            {
                // Switched already, a relaunch that could not be made or no
                // host to make one, or the fallback is the model at its
                // limit: the reset is waited out.
                return wait_out();
            }
            TurnEndAction::Restart {
                why: Restart::Model {
                    to: fallback.clone(),
                },
                rule_id: RULE_MODEL_FALLBACK,
                otherwise: Box::new(wait_out()),
            }
        }
        WallKind::Context => {
            if !cfg.compact_on_context_wall {
                return esc(format!(
                    "context full: {msg} (compact_on_context_wall is off)"
                ));
            }
            if let Some(wait) = attempts.checked_sub(1).and_then(|n| retry_wait(state, n)) {
                let due = later(since, wait);
                if now < due {
                    return TurnEndAction::WaitUntil {
                        until: due,
                        why: format!("context still full after /compact ({attempts})"),
                    };
                }
            }
            budgeted(TurnEndAction::TypeCommand {
                command: "/compact".to_string(),
                rule_id: RULE_COMPACT,
                then: Then::Continue,
            })
        }
        // Decided ahead of every gate in `decide_turn_end`; the arm keeps
        // the match exhaustive with the same act.
        WallKind::Memory => memory_restart(cfg, reading),
        WallKind::Auth => {
            if attempts > 0 {
                return TurnEndAction::Nothing;
            }
            // An API failure the worker cannot retry past: typed wherever API
            // errors are retried (`retry_api_errors`, on by default — the
            // window's host and `drive watch` alike).
            if !cfg.retry_api_errors {
                return esc(format!("the login is gone: {msg}"));
            }
            budgeted(TurnEndAction::TypeCommand {
                command: "/login".to_string(),
                rule_id: RULE_LOGIN,
                then: Then::Escalate("finish sign-in in the browser".to_string()),
            })
        }
    }
}

/// Claude Code's critical-memory banner (2026-09-24: a worker at 38.8 GiB
/// resident that read no input for 2h41m). Nothing typed into it is read
/// and no reset ends it; the one remedy is a restart, and the banner names
/// it — so it is TAKEN (D3): [`TurnEndAction::Restart`], the session's host
/// ending the agent at its idle point and relaunching it on its own
/// conversation, then telling it to carry on. The DECIDER says it at any
/// point it is asked about, ahead of the gates a typed act waits on. The
/// LOOP asks only at a point: under a running spinner — where Claude Code
/// draws the banner, and where the incident sat — it waits for the turn to
/// end, and the server's own `wall:memory` menu row is the notice there
/// (`aterm-gui`'s `status_item`). `[harness] relaunch = false` limits it to
/// the escalation with its remedy, and so does a loop no host restarts for
/// (`drive watch`), which escalates the same `reason`.
fn memory_restart(cfg: &SupervisorConfig, reading: &TurnEndReading) -> TurnEndAction {
    let reason = format!(
        "memory critical: restart it, then resume with {}: {}",
        aterm_phase::resume_hint(aterm_phase::Program::Claude).unwrap_or("its resume command"),
        reading.wall_message
    );
    if !cfg.relaunch {
        return TurnEndAction::Escalate {
            reason: format!("{reason} (relaunch is off)"),
        };
    }
    if !reading.restartable {
        return TurnEndAction::Escalate { reason };
    }
    TurnEndAction::Restart {
        why: Restart::Memory,
        rule_id: RULE_MEMORY_RESTART,
        otherwise: Box::new(TurnEndAction::Escalate { reason }),
    }
}

#[cfg(test)]
#[path = "turn_end_tests.rs"]
mod tests;
