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
//! **Continue** (owner decision 2 — the harness audit's first blocker: 60% of
//! the owner's typed prompts were a rote `keep going` after a turn ended
//! early). At an authoritative idle point — or a question that is an offer
//! (`want me to…`, `shall I…`, `next steps:`) — after at least
//! [`TurnEndTiming::min_work`] of busy work (or right after a continuation of
//! this policy's, whose yield is what is being judged), with no box, no wall
//! and nothing typed in the composer: accept the worker's own suggestion when
//! it is a continuation ([`suggestion_allowed`]), else type
//! `cfg.continue_text` (with the standing rules when a rules file is set).
//! A worker that asks for a decision ([`STOP_PHRASES`], a choice between
//! listed options with no recommendation, an offer to do something
//! destructive — [`DESTRUCTIVE_WORDS`] — any other question) is escalated
//! instead; so is a spent budget (`cfg.continue_per_hour` typed acts in any
//! [`TurnEndTiming::window`]) and two continuations in a row that each
//! produced less than `min_work` ("worker reports done").
//!
//! **Walls** ([`aterm_phase::wall`]). Overload and a retryable API error:
//! wait out [`TurnEndTiming::retry_backoff`] (1, 5, 15 min) from each
//! appearance, continue after each, escalate past the last. A usage window:
//! nothing when the vendor says it goes on by itself, else continue a
//! [`TurnEndTiming::reset_grace`] past the reset it names (the old resume
//! PROBE question is gone: the continuation is what the owner typed by hand),
//! and after a continuation that hit the wall again, [`TurnEndTiming::
//! limit_backoff`]. A model bucket (owner decision 3): `/model <fallback>`,
//! then continue; `/model <the bucket's model>` again at the bucket's reset,
//! when both are known. A full context: `/compact`, then continue. A lost
//! login: `/login`, then escalate "finish sign-in in the browser" (under
//! `retry_api_errors`: a `watch` that retries nothing escalates it). Money,
//! and a model bucket that asks consent to spend credits, escalate.
//!
//! **Never** while a box, the session survey or a wall's wait is up, on a
//! non-authoritative reading (a shell, a Codex screen outside its box), with
//! text in the composer — the human typing — or on a turn a person stopped
//! with Esc (`⎿  Interrupted · What should Claude do instead?`).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use aterm_phase::phase::{Phase, has_composer_frame, is_done_row, last_said_row, status_row};
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
/// `/model <fallback>` on a model bucket, and the continuation after it.
pub const RULE_MODEL_FALLBACK: &str = "model-fallback@v1";
/// `/model <the bucket's model>` again at the bucket's reset.
pub const RULE_MODEL_RESTORE: &str = "model-restore@v1";
/// `/compact` on a full context, and the continuation after it.
pub const RULE_COMPACT: &str = "context-compact@v1";
/// `/login` on a lost login.
pub const RULE_LOGIN: &str = "auth-login@v1";

/// The rules whose continuation's yield counts toward "worker reports done".
fn is_continue_rule(rule: &str) -> bool {
    rule == RULE_CONTINUE || rule == RULE_SUGGESTION
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

/// Words a worker asks a person for a decision with: the point is escalated,
/// never continued. Matched lowercased, anywhere in the worker's last words.
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
    "which option",
    "which would you prefer",
    "which do you prefer",
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
    "let me know which",
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

/// Words that make an OFFER one a person must answer: an offer to do one of
/// these is escalated, never continued — `keep going` would read as
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
    "drop",
    "dropping",
    "remove",
    "removing",
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
    /// The busy work a turn must have done for its end to be continued.
    pub min_work: Duration,
    /// The wait before each retry of an overload or a retryable API error,
    /// counted from the wall's appearance; past the last, escalate.
    pub retry_backoff: Vec<Duration>,
    /// How long past a usage window's reset the continuation waits.
    pub reset_grace: Duration,
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
}

impl Default for TurnEndTiming {
    fn default() -> Self {
        let min = |m: u64| Duration::from_secs(m * 60);
        Self {
            min_work: min(2),
            retry_backoff: vec![min(1), min(5), min(15)],
            reset_grace: Duration::from_secs(60),
            limit_backoff: vec![min(10), min(30)],
            window: min(60),
            take_within: Duration::from_secs(30),
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
    /// Text is typed: a human's draft, never typed over.
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
    /// The turn answers the live upgrade's announcement ([`upgrade_owns`]):
    /// the sweep owns the session until it restarts it, and nothing is typed.
    pub upgrading: bool,
}

/// Whether the live agent upgrade (`harness::upgrade_drive`) owns this point:
/// the transcript's last user message is its announcement
/// ([`crate::harness::upgrade::ANNOUNCE_HEAD`], never the `Upgraded:`
/// continuation, which asks the worker to go on), or the worker's last words
/// carry its READY marker. The sweep asks the worker to wind down and waits
/// for quiet; a `keep going` typed into the wind-down restarted work and
/// deferred the upgrade, or ran up a false "worker reports done" (the
/// reliability review of 2026-09-24).
#[must_use]
pub fn upgrade_owns(rows: &[String], said_tail: Option<&str>) -> bool {
    use crate::harness::upgrade::{ANNOUNCE_HEAD, READY_PREFIX};
    if said_tail.is_some_and(|t| t.contains(READY_PREFIX)) {
        return true;
    }
    let end = aterm_phase::phase::transcript_end(rows).min(rows.len());
    rows[..end]
        .iter()
        .rev()
        .find(|r| r.starts_with('❯'))
        .and_then(|r| r.strip_prefix('❯'))
        .is_some_and(|r| r.trim_start().starts_with(ANNOUNCE_HEAD))
}

impl TurnEndReading {
    /// The reading of one screen: `aterm-phase`'s [`aterm_phase::Reading`]
    /// of `rows` (read with the cursor's column), whether a draft is typed
    /// (the loop's `typed_draft`), the work since the last point, the wall's
    /// reset on the loop's clock, and the rules.
    #[must_use]
    pub fn of(
        reading: &aterm_phase::Reading,
        rows: &[String],
        typed: bool,
        worked: Option<Duration>,
        reset_at: Option<Instant>,
        rules: Option<String>,
    ) -> Self {
        let composer = if !has_composer_frame(rows) {
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
            worked: worked.map(|w| w.max(done_row_work(rows).unwrap_or_default())),
            rules,
            pending_input: last_said_row(rows).is_some_and(|r| r.trim_start().starts_with('❯')),
            interrupted: aterm_phase::interrupted(rows),
            upgrading: upgrade_owns(rows, reading.said_tail.as_deref()),
        }
    }

    /// A turn has ended here and nothing blocks typing into it: the phase
    /// is idle, a question or a limit — read by a reader whose word is
    /// evidence — no box or survey is up, and no input waits unanswered.
    fn at_point(&self) -> bool {
        matches!(
            self.phase,
            Phase::Idle | Phase::Question | Phase::Limited { .. }
        ) && self.authoritative
            && !self.survey
            && !self.pending_input
    }

    fn composer_free(&self) -> bool {
        matches!(self.composer, Composer::Empty | Composer::Placeholder(_))
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
    /// Hand the point to a person.
    Escalate {
        reason: String,
    },
}

impl TurnEndAction {
    /// The rule an act is taken under (`None` for no act).
    #[must_use]
    pub fn rule_id(&self) -> Option<&'static str> {
        match self {
            TurnEndAction::Type { rule_id, .. }
            | TurnEndAction::Accept { rule_id, .. }
            | TurnEndAction::TypeCommand { rule_id, .. } => Some(rule_id),
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
    Other,
}

fn class_of(kind: WallKind) -> WallClass {
    match kind {
        WallKind::Overloaded
        | WallKind::ApiError {
            retryable: true, ..
        } => WallClass::Retry,
        WallKind::UsageSession | WallKind::UsageWeekly => WallClass::Usage,
        WallKind::ModelBucket { .. } => WallClass::Model,
        WallKind::Context => WallClass::Context,
        WallKind::Auth => WallClass::Auth,
        WallKind::Spend | WallKind::ApiError { .. } => WallClass::Other,
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
    /// This policy's continuations in a row that each produced less than
    /// [`TurnEndTiming::min_work`].
    short: u32,
    wall: Option<WallTrack>,
    /// A command's rule whose continuation is owed at the next point.
    after: Option<&'static str>,
    model: Option<ModelSwitch>,
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
            wall: None,
            after: None,
            model: None,
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

    /// Continuations in a row that each produced less than `min_work`.
    #[must_use]
    pub fn short_streak(&self) -> u32 {
        self.short
    }

    /// The model switch in force, if any.
    #[must_use]
    pub fn model_switch(&self) -> Option<&ModelSwitch> {
        self.model.as_ref()
    }

    /// Fold what a point shows into the state, BEFORE [`decide_turn_end`] is
    /// asked about it: the yield of the act typed last (a continuation that
    /// produced less than `min_work` lengthens the short streak, one that
    /// produced more ends it), someone else's turn in between (`worked`
    /// with nothing awaited: the streak ends when it was real work), and the
    /// wall on the screen (a new kind opens a track; the same kind again
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
    /// busy read (`/model` answers at once).
    pub fn observe(&mut self, reading: &TurnEndReading, now: Instant) {
        if matches!(reading.phase, Phase::Busy | Phase::Prompt) || reading.pending_input {
            return;
        }
        if let Some(a) = &mut self.awaiting
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
        match (awaited, reading.worked) {
            (Some(rule), worked) => {
                let d = worked.unwrap_or_default();
                self.followed = Some(rule);
                self.point_worked = d;
                if is_continue_rule(rule) {
                    self.short = if d < min { self.short + 1 } else { 0 };
                }
            }
            (None, Some(d)) => {
                self.followed = None;
                self.point_worked = d;
                if d >= min {
                    self.short = 0;
                }
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
                }
            },
            // The wall has left the screen: gone for good once the worker
            // worked (or our retry was taken). With no work — a human's
            // `/login` or `/model` output over it — a usage track stays, and
            // the continuation goes at once ([`decide_turn_end`]).
            None => {
                if reading.worked.is_some() || retried {
                    self.wall = None;
                }
            }
        }
    }

    /// Record an act the loop TYPED (a skipped or refused one is not): the
    /// budget, the act awaited, what it leaves owed, and — for a wall's act
    /// — the track's attempts and the model switch.
    pub fn acted(&mut self, action: &TurnEndAction, reading: &TurnEndReading, now: Instant) {
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
            RULE_API_RETRY | RULE_LIMIT_RESUME | RULE_MODEL_FALLBACK | RULE_COMPACT | RULE_LOGIN
        ) && let Some(t) = &mut self.wall
        {
            t.attempts += 1;
        }
        match (rule, action) {
            (RULE_MODEL_FALLBACK, TurnEndAction::TypeCommand { command, .. }) => {
                let to = command
                    .strip_prefix("/model ")
                    .unwrap_or(command)
                    .to_string();
                self.model = Some(ModelSwitch {
                    from: bucket_model(&reading.wall_message),
                    to,
                    back_at: reading.reset_at,
                });
            }
            (RULE_MODEL_RESTORE, _) => self.model = None,
            _ => {}
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
    /// A question for a person: escalated.
    Question,
    /// A decision asked for: escalated, with why.
    Stop(String),
}

/// The worker's last words, judged ([`Said`]). `tail` comes one line per
/// SCREEN ROW ([`aterm_phase::said_tail`]), so a soft wrap is a line break
/// that means nothing: phrases are matched on the rows joined with single
/// spaces, and the question is the last SENTENCE of the last unit — the
/// rows from the last enumerated option row on, joined — never the last
/// row (lane B2's review: `Want me to keep the old API⏎or drop it?` and a
/// stop phrase split by a wrap were continued). A stop phrase anywhere
/// wins; then an offer to do something destructive ([`DESTRUCTIVE_WORDS`]
/// anywhere after the FIRST offer phrase — a benign offer after a
/// destructive one does not hide it) is a stop, and so is any question
/// that names a destructive act, offer or not (`Delete the stale branches
/// now? (y/n)`); then a last sentence that asks (`?`, or a trailing `(y/n)`
/// / `[y/N]` / `(yes/no)`, a trailing parenthetical aside after it not
/// counting): a choice between options with no recommendation (`should I …
/// or …`, `… A or B?`, two or more listed options) is a stop, an offer is
/// an offer, anything else a question; a tail that does not ask is an offer
/// when it opens next steps, else plain. (The safety review of 2026-09-24:
/// each of those shapes was continued, and `keep going` reads as yes.)
#[must_use]
pub fn classify_said(tail: Option<&str>) -> Said {
    let Some(tail) = tail else {
        return Said::Plain;
    };
    let lower = yes_no_asks(&tail.to_lowercase().replace('’', "'"));
    let flat = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(p) = STOP_PHRASES.iter().find(|p| flat.contains(*p)) {
        return Said::Stop(format!("the worker said \"{p}\""));
    }
    let recommends = RECOMMEND_PHRASES.iter().any(|p| flat.contains(p));
    let offer_at = OFFER_PHRASES.iter().filter_map(|p| flat.find(p)).min();
    if let Some(at) = offer_at
        && let Some(w) = destructive_word(&flat[at..])
    {
        return Said::Stop(format!("the worker offers to {w}: a person answers that"));
    }
    if let Some(w) = questions(&flat).into_iter().find_map(destructive_word) {
        return Said::Stop(format!(
            "the worker asks whether to {w}: a person answers that"
        ));
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
    if !asks {
        return if offer { Said::Offer } else { Said::Plain };
    }
    let listed = lines.iter().filter(|l| is_option_line(l)).count();
    let question = last_sentence(unit);
    let either = question.contains(" or ");
    if !recommends && (listed >= 2 || either) {
        return Said::Stop(if listed >= 2 {
            "the worker listed options with no recommendation".to_string()
        } else {
            "the worker asks to choose between options".to_string()
        });
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

/// The first of [`DESTRUCTIVE_WORDS`] in `text`, as a whole word (a phrase
/// as whole words).
fn destructive_word(text: &str) -> Option<&'static str> {
    let words: Vec<&str> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .collect();
    DESTRUCTIVE_WORDS.iter().copied().find(|d| {
        let want: Vec<&str> = d.split(' ').map(|w| w.trim_start_matches('-')).collect();
        words.windows(want.len()).any(|win| {
            win.iter()
                .map(|w| w.trim_start_matches('-'))
                .eq(want.iter().copied())
        })
    })
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
    match rules.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => format!("{} (standing rules: {r})", cfg.continue_text),
        None => cfg.continue_text.clone(),
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
    // A person stopped the turn (Esc): a stop, never a point to continue —
    // nor one to escalate; the person is at the keyboard. A point that
    // answers the live upgrade's announcement is the sweep's.
    if reading.interrupted || reading.upgrading {
        return TurnEndAction::Nothing;
    }
    // An act typed and its point not judged yet ([`TurnEndState::observe`]):
    // decide again when it is due; past that with the act still not taken
    // — its text submitted and unanswered — a person is told, never a latch.
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
        return TurnEndAction::Escalate {
            reason: format!(
                "the {} act was typed {} s ago and the worker has not taken it",
                a.rule,
                now.saturating_duration_since(a.at).as_secs()
            ),
        };
    }
    if !reading.at_point() || !reading.composer_free() {
        return TurnEndAction::Nothing;
    }
    let budgeted = |act: TurnEndAction| {
        let used = state.acts_in_window(now);
        if used >= cfg.continue_per_hour as usize {
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
        && (t.class == WallClass::Usage
            || (t.class == WallClass::Model && cfg.model_fallback.is_none()))
        && t.attempts == 0
        && cfg.resume_limits
    {
        return continue_as(RULE_LIMIT_RESUME);
    }
    let cont = continue_policy(state, reading, cfg, &budgeted);
    // The bucket's reset has come: switch back first, the continuation owed
    // after it when this point was to be continued.
    if let Some(m) = &state.model
        && let (Some(from), Some(at)) = (&m.from, m.back_at)
        && now >= at
        && !matches!(cont, TurnEndAction::Escalate { .. })
    {
        let then = if matches!(
            cont,
            TurnEndAction::Type { .. } | TurnEndAction::Accept { .. }
        ) {
            Then::Continue
        } else {
            Then::Nothing
        };
        return budgeted(TurnEndAction::TypeCommand {
            command: format!("/model {from}"),
            rule_id: RULE_MODEL_RESTORE,
            then,
        });
    }
    cont
}

/// The continue policy at a point with no wall.
fn continue_policy(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> TurnEndAction {
    if !cfg.continue_policy {
        return TurnEndAction::Nothing;
    }
    let ours = state.followed.is_some_and(is_continue_rule);
    if !ours && state.point_worked < state.timing.min_work {
        return TurnEndAction::Nothing;
    }
    // A question the reader could not read the words of is never taken for
    // "nothing asked".
    if reading.phase == Phase::Question && reading.said_tail.is_none() {
        return TurnEndAction::Escalate {
            reason: "the worker asked something its last words do not show".to_string(),
        };
    }
    match classify_said(reading.said_tail.as_deref()) {
        Said::Stop(why) => return TurnEndAction::Escalate { reason: why },
        Said::Question => {
            return TurnEndAction::Escalate {
                reason: "the worker asked a question".to_string(),
            };
        }
        Said::Offer | Said::Plain => {}
    }
    if state.short >= 2 {
        return TurnEndAction::Escalate {
            reason: format!(
                "worker reports done: two continuations in a row each produced under {} s of work",
                state.timing.min_work.as_secs()
            ),
        };
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
    let backoff = |i: usize| back.get(i.min(back.len().saturating_sub(1))).copied();
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
    match kind {
        WallKind::Overloaded
        | WallKind::ApiError {
            retryable: true, ..
        } => {
            if !cfg.retry_api_errors {
                return esc(format!("{}: {msg} (retry_api_errors is off)", kind.name()));
            }
            let Some(&wait) = state.timing.retry_backoff.get(attempts) else {
                return esc(format!("{} after {attempts} retries: {msg}", kind.name()));
            };
            let due = later(since, wait);
            if now < due {
                return TurnEndAction::WaitUntil {
                    until: due,
                    why: format!(
                        "{} retry {} of {}",
                        kind.name(),
                        attempts + 1,
                        state.timing.retry_backoff.len()
                    ),
                };
            }
            continue_as(RULE_API_RETRY)
        }
        WallKind::ApiError { code, .. } => esc(format!(
            "API error {}: {msg}",
            code.map_or_else(|| "-".to_string(), |c| c.to_string())
        )),
        WallKind::UsageSession | WallKind::UsageWeekly => {
            if !cfg.resume_limits || reading.resumes_by_itself {
                return TurnEndAction::Nothing;
            }
            resume_at_reset(state, reading, now, since, attempts, kind, continue_as)
        }
        WallKind::ModelBucket { consent: true } => esc(format!(
            "the vendor asks consent to go on on usage credits: {msg}"
        )),
        WallKind::ModelBucket { consent: false } => {
            // No model to switch to: a bucket resets like a usage window,
            // and is waited out the same way when limits are resumed.
            let Some(fallback) = &cfg.model_fallback else {
                if cfg.resume_limits {
                    return resume_at_reset(
                        state,
                        reading,
                        now,
                        since,
                        attempts,
                        kind,
                        continue_as,
                    );
                }
                return esc(format!("model-bucket limit, no fallback model: {msg}"));
            };
            if let Some(m) = &state.model {
                return esc(format!(
                    "model-bucket limit again after switching to {}: {msg}",
                    m.to
                ));
            }
            if bucket_model(msg).as_deref() == Some(fallback.as_str()) {
                return esc(format!(
                    "the fallback model ({fallback}) is the one at its limit: {msg}"
                ));
            }
            budgeted(TurnEndAction::TypeCommand {
                command: format!("/model {fallback}"),
                rule_id: RULE_MODEL_FALLBACK,
                then: Then::Continue,
            })
        }
        WallKind::Context => {
            if !cfg.compact_on_context_wall {
                return esc(format!("context full: {msg}"));
            }
            if attempts > 0 {
                return esc(format!("context still full after /compact: {msg}"));
            }
            budgeted(TurnEndAction::TypeCommand {
                command: "/compact".to_string(),
                rule_id: RULE_COMPACT,
                then: Then::Continue,
            })
        }
        WallKind::Auth => {
            if attempts > 0 {
                return TurnEndAction::Nothing;
            }
            // An API failure the worker cannot retry past: typed only where
            // API errors are retried (the host's policy, never the CLI's).
            if !cfg.retry_api_errors {
                return esc(format!("the login is gone: {msg}"));
            }
            budgeted(TurnEndAction::TypeCommand {
                command: "/login".to_string(),
                rule_id: RULE_LOGIN,
                then: Then::Escalate("finish sign-in in the browser".to_string()),
            })
        }
        WallKind::Spend => esc(format!("spend limit: {msg}")),
    }
}

#[cfg(test)]
#[path = "turn_end_tests.rs"]
mod tests;
