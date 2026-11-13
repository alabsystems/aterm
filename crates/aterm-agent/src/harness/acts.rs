// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Harness act and landing projections shared across the wire.

use crate::supervise::approvals::Outcome;
use crate::supervise::policy::approval::{
    PriorityWord, RULE_ANSWER_LAND, RULE_ANSWER_LONG_TERM, RULE_ANSWER_SHORT_TERM, RULE_PLAN,
    RULE_PROSE_LONG_TERM, RULE_PROSE_SHORT_TERM,
};
use crate::supervise::policy::credential_need::RULE_CREDENTIAL;
use crate::supervise::policy::go_ahead::RULE_GO_AHEAD;
use crate::supervise::policy::question::{RULE_ANSWER_RECOMMENDED, is_question_rule};
use crate::supervise::policy::turn_end::{
    RULE_ANSWER, RULE_API_BACK, RULE_API_CUTOFF, RULE_API_RETRY, RULE_COMPACT, RULE_CONTINUE,
    RULE_DONE_CHECK, RULE_HOST_TURN, RULE_LAND, RULE_LAND_ANSWER, RULE_LAND_CHECK, RULE_LAND_NUDGE,
    RULE_LIMIT_RESUME, RULE_LOGIN_BACK, RULE_OPEN_ITEMS, RULE_OUTAGE_RESUME, RULE_SUGGESTION,
    RULE_UNCOMMITTED,
};

/// One act of the harness's, as the Autopilot chip names it for a moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Act {
    /// A box approved (`approved` under a box's rule).
    Approved,
    /// A box declined with a reason (`declined`).
    Declined,
    /// A question answered: a dialog's keys (one label for the whole dialog) or a prose answer.
    Answered,
    /// The work kept moving.
    Continued,
    /// Carried past a wall: a usage reset, an API error, a cut reply.
    Resumed,
    /// Land's instruction typed (`land@v1`).
    Landing,
    /// A land nudge typed (`land-nudge@v1`).
    Nudged,
    /// The land check typed (`land-check@v1`): the agent audits its own report, answering
    /// `LANDED` or what is left.
    LandCheck,
}

impl Act {
    /// Every act, in display order.
    pub const ALL: [Act; 8] = [
        Act::Approved,
        Act::Declined,
        Act::Answered,
        Act::Continued,
        Act::Resumed,
        Act::Landing,
        Act::Nudged,
        Act::LandCheck,
    ];

    /// The act's glyph, drawn before its word.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Act::Approved => "\u{2713}",
            Act::Declined => "\u{2717}",
            Act::Answered => "?",
            Act::Continued => "\u{21bb}",
            Act::Resumed => "\u{29d7}",
            Act::Landing | Act::Nudged | Act::LandCheck => "\u{2913}",
        }
    }

    /// The act's word, drawn after its glyph and a space (`✓ approved`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Act::Approved => "approved",
            Act::Declined => "declined",
            Act::Answered => "answered",
            Act::Continued => "kept going",
            Act::Resumed => "resumed",
            Act::Landing => "landing",
            Act::Nudged => "nudged",
            Act::LandCheck => "land check",
        }
    }

    /// The act in words a screen reader says, no glyph.
    #[must_use]
    pub const fn spoken(self) -> &'static str {
        match self {
            Act::Landing => "started landing",
            act => act.word(),
        }
    }
}

/// AN ACT AS THE LOOP TELLS THE WINDOW'S HOST (`IdleHost::act_taken`): the act, and whether
/// aterm typed it at the prompt, opening a turn of its own, where a box's approval and a
/// dialog's keys answer inside the turn that asked. A prose answer (`go-ahead@v1`, a prose
/// question's rule, `land-answer@v1` typed) is [`Act::Answered`] as a dialog's keys are, and only
/// this says which it was (the second review of the post-landing fixes, 2026-11-01): the
/// window's book ends the land check's audit at a turn aterm opened past it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TakenAct {
    /// The act the chip lights for a moment.
    pub act: Act,
    /// Whether aterm typed it at the prompt ([`Outcome::Typed`]).
    pub opens_turn: bool,
}

impl TakenAct {
    /// `act`, ledgered as `outcome`.
    #[must_use]
    pub fn of(act: Act, outcome: Outcome) -> Self {
        Self {
            act,
            opens_turn: outcome == Outcome::Typed,
        }
    }
}

/// A counter of what the harness did ([`super::stats::HarnessStats`]) one ledger row adds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Counter {
    /// [`super::stats::HarnessStats::approved`].
    Approved,
    /// [`super::stats::HarnessStats::declined`].
    Declined,
    /// [`super::stats::HarnessStats::answered`].
    Answered,
    /// [`super::stats::HarnessStats::continued`].
    Continued,
    /// [`super::stats::HarnessStats::resumed`].
    Resumed,
    /// [`super::stats::HarnessStats::land_prompts`].
    LandPrompts,
}

/// What one ledger row is ([`sort`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Sorted {
    /// The act the chip lights for a moment.
    pub act: Option<Act>,
    /// The counter the page adds the row to.
    pub counter: Option<Counter>,
}

/// Sort a ledger row (`rule_id`, `outcome`, `command`) into its act and counter.
#[must_use]
pub fn sort(rule_id: &str, outcome: Outcome, command: &str) -> Sorted {
    let sorted = |act, counter| Sorted {
        act: Some(act),
        counter,
    };
    match outcome {
        Outcome::Approved if is_dialog_key(rule_id) => sorted(Act::Answered, None),
        Outcome::Approved => sorted(Act::Approved, Some(Counter::Approved)),
        Outcome::Declined => sorted(Act::Declined, Some(Counter::Declined)),
        Outcome::Typed => match typed_act(rule_id, command) {
            Some((act, counter)) => sorted(act, Some(counter)),
            None => Sorted::default(),
        },
        Outcome::Skipped | Outcome::Escalated | Outcome::Deferred | Outcome::Waiting => {
            Sorted::default()
        }
    }
}

fn is_dialog_key(rule: &str) -> bool {
    is_question_rule(rule) || rule == RULE_LAND_ANSWER
}

fn is_prose_answer(rule: &str) -> bool {
    rule == RULE_LAND_ANSWER
        || rule == RULE_GO_AHEAD
        || rule == RULE_CREDENTIAL
        || PriorityWord::ALL
            .into_iter()
            .any(|p| p.prose_rule_id() == rule)
}

fn typed_act(rule: &str, command: &str) -> Option<(Act, Counter)> {
    if matches!(
        rule,
        RULE_CONTINUE
            | RULE_POLICY_CONTINUE
            | RULE_SUGGESTION
            | RULE_DONE_CHECK
            | RULE_OPEN_ITEMS
            | RULE_UNCOMMITTED
            | crate::supervise::policy::cruise_carry::RULE_BACKGROUND
    ) {
        Some((Act::Continued, Counter::Continued))
    } else if is_prose_answer(rule) {
        Some((Act::Answered, Counter::Answered))
    } else if rule == RULE_LAND {
        Some((Act::Landing, Counter::LandPrompts))
    } else if rule == RULE_LAND_NUDGE {
        Some((Act::Nudged, Counter::LandPrompts))
    } else if rule == RULE_LAND_CHECK {
        Some((Act::LandCheck, Counter::LandPrompts))
    } else if matches!(
        rule,
        RULE_LIMIT_RESUME
            | RULE_API_RETRY
            | RULE_API_BACK
            | RULE_API_CUTOFF
            | RULE_OUTAGE_RESUME
            | RULE_LOGIN_BACK
    ) || (rule == RULE_COMPACT && !command.starts_with('/'))
    {
        Some((Act::Resumed, Counter::Resumed))
    } else {
        None
    }
}

/// The session log's `act` mark for a ledger row.
#[must_use]
pub fn log_mark(rule_id: &str, outcome: Outcome, command: &str) -> Option<String> {
    let word = if outcome == Outcome::Escalated {
        "escalate"
    } else {
        match sort(rule_id, outcome, command).act? {
            Act::Approved => "approve",
            Act::Declined => "deny",
            Act::Answered => "answer",
            Act::Continued | Act::Resumed => "continue",
            Act::Landing | Act::Nudged | Act::LandCheck => "dispatch",
        }
    };
    let rule = log_rule(rule_id);
    Some(if rule.is_empty() || rule == "-" {
        format!("act word={word}")
    } else {
        format!("act word={word} rule={rule}")
    })
}

fn log_rule(rule_id: &str) -> String {
    rule_id
        .chars()
        .map(|c| if c == '@' { '.' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
        .take(LOG_RULE_MAX)
        .collect()
}

/// The act words of a self-supervised agent's own acts.
#[must_use]
pub fn self_claim_act(act: &str) -> Option<(&'static str, String)> {
    let word = match act {
        RULE_APPROVE_ALL | RULE_PLAN => "approve",
        RULE_ANSWER_RECOMMENDED
        | RULE_ANSWER_LONG_TERM
        | RULE_ANSWER_SHORT_TERM
        | RULE_ANSWER_LAND
        | RULE_LAND_ANSWER
        | RULE_GO_AHEAD
        | RULE_CREDENTIAL
        | RULE_ANSWER
        | RULE_PROSE_LONG_TERM
        | RULE_PROSE_SHORT_TERM => "answer",
        RULE_CONTINUE | RULE_POLICY_CONTINUE | RULE_LIMIT_RESUME | RULE_DONE_CHECK
        | RULE_API_RETRY | RULE_API_BACK | RULE_API_CUTOFF | RULE_COMPACT => "continue",
        RULE_UPGRADE => "relaunch",
        RULE_HOST_TURN | RULE_LAND | RULE_LAND_NUDGE | RULE_LAND_CHECK => "dispatch",
        RULE_LANDED => LANDED_WORD,
        _ => return None,
    };
    Some((word, log_rule(act)))
}

/// Whether a self-claim's act types a turn (Autopilot Land).
#[must_use]
pub fn self_claim_types_turn(act: &str) -> bool {
    act == RULE_HOST_TURN || act == RULE_PLAN || typed_act(act, "").is_some()
}

/// A PROVEN SELF-CLAIM'S ACT ON THE CHIP AND IN ITS OWN COUNTS: what the holder's `meta set
/// harness_act <act>` lights and adds to, as aterm's own typed act under that rule does
/// ([`typed_act`]), and its box acts as aterm's own approval of the box lights them ([`sort`]):
/// `approve-all@v1` and the plan box's yes ([`RULE_PLAN`]) approved, a question box's answer
/// under any priority ([`is_question_rule`]) answered. Each of those counts once, where aterm
/// counts its own dialog's answer from its journal's choice. None for its landing
/// ([`RULE_LANDED`]: the face says it, and the counts book it apart), its re-exec
/// ([`RULE_UPGRADE`]), a point left to a person, a mail turn's receipt, or any other act, which
/// type nothing in the person's name.
#[must_use]
pub fn self_claim_sort(act: &str) -> Sorted {
    let sorted = |act, counter| Sorted {
        act: Some(act),
        counter: Some(counter),
    };
    if act == RULE_APPROVE_ALL || act == RULE_PLAN {
        sorted(Act::Approved, Counter::Approved)
    } else if is_question_rule(act) {
        sorted(Act::Answered, Counter::Answered)
    } else {
        typed_act(act, "").map_or_else(Sorted::default, |(act, counter)| sorted(act, counter))
    }
}

/// THE NAME A SELF-CLAIM'S HOLDER ACTS UNDER: `holder` without the `@<pid>` its proof names
/// (`codex-alab` for `codex-alab@4242`), so every process of one agent is one name in the
/// words and the counts; a holder with no such suffix is its own name.
#[must_use]
pub fn claimant_name(holder: &str) -> &str {
    match holder.rsplit_once('@') {
        Some((name, pid))
            if !name.is_empty() && !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) =>
        {
            name
        }
        _ => holder,
    }
}

/// The session log's act word for a self-claim's landing ([`RULE_LANDED`], [`self_claim_act`]).
pub const LANDED_WORD: &str = "landed";

/// The act word of the hosted lanes' relaunch and upgrade rows ([`relaunch_act`]).
pub const RELAUNCH_WORD: &str = "relaunch";

/// The hosted lanes' act mark.
#[must_use]
pub fn relaunch_act(row: &str, step: &str) -> Option<String> {
    if !super::upgrade_drive::moved(step) {
        return None;
    }
    let rule = log_rule(row);
    (!rule.is_empty()).then_some(rule)
}

/// How long one of the harness's own labels lasts (`ttl=`, seconds): its moment's words, gone
/// before the work beneath them grows old.
pub const HARNESS_LABEL_TTL_S: u64 = 30;

/// The harness's label once it relaunched a session's agent.
pub const RESTARTING_LABEL: &str = "restarting";

/// The harness's label once it typed its resume after a full disk.
pub const DISK_RESUME_LABEL: &str = "resuming after disk";

/// The harness's label once it typed its ask to free disk space (`disk-free@v1`).
pub const DISK_FREE_LABEL: &str = "asked to free disk";

/// The harness's label once it typed its carry-on after a usage wall.
pub const LIMIT_RESUME_LABEL: &str = "resuming after limit";

const UPGRADING_LABEL: &str = "upgrading";

/// The longest version an `upgrading to <ver>` label names: 32 graphemes in all, the `mark`
/// verb's bound on a label.
const LABEL_VERSION_MAX: usize = 19;

/// The harness's own label for a hosted lane's row `row` at its `step`, once a new agent came up
/// ([`super::upgrade_drive::started`]): `upgrading to <ver>` for an upgrade's restart (`to`
/// without its build metadata or `(<source>)`; plain `upgrading` when that is not a version),
/// else `restarting`; `None` for a step that started nothing. The words are closed templates
/// and a version's characters, never the worker's text.
#[must_use]
pub fn relaunch_label(row: &str, step: &str, to: &str) -> Option<String> {
    if !super::upgrade_drive::started(step) {
        return None;
    }
    if !matches!(row, "upgrade" | "upgrade-at-break") {
        return Some(RESTARTING_LABEL.to_string());
    }
    let ver = to.split(['+', '(']).next().unwrap_or_default();
    let named = ver.len() <= LABEL_VERSION_MAX
        && ver.bytes().any(|b| b.is_ascii_digit())
        && ver
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
    Some(if named {
        format!("{UPGRADING_LABEL} to {ver}")
    } else {
        UPGRADING_LABEL.to_string()
    })
}

/// The label the loop writes beside a ledger row of its own (`rule_id`, `outcome`): its
/// carry-on after a usage wall; `None` for every other row.
#[must_use]
pub fn loop_label(rule_id: &str, outcome: Outcome) -> Option<&'static str> {
    (outcome == Outcome::Typed && rule_id == RULE_LIMIT_RESUME).then_some(LIMIT_RESUME_LABEL)
}

/// The `mark` verb's arguments after `mark -` for the harness's label `words`:
/// `label text=<pct> ttl=<HARNESS_LABEL_TTL_S>`.
#[must_use]
pub fn label_mark(words: &str) -> String {
    format!(
        "label text={} ttl={HARNESS_LABEL_TTL_S}",
        aterm_codec::pct::encode(words)
    )
}

const RULE_APPROVE_ALL: &str = "approve-all@v1";
/// Codex-alab's re-exec onto a re-linked build (the proof's `upgrade` leg).
pub const RULE_UPGRADE: &str = "upgrade@v1";
/// Codex-alab's bounded continuation of independent allowed work after a cybersecurity refusal.
pub const RULE_POLICY_CONTINUE: &str = "policy-continue@v1";
/// Codex-alab's landing under its own Land (the contract's Land row).
pub const RULE_LANDED: &str = "landed@v1";

const LOG_RULE_MAX: usize = 40;

/// The task's face, told by the loop as it moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaskFace {
    /// Nothing ended: the work goes on, or a landing is under way.
    Open,
    /// A clean defense at a land act's yield.
    Landed,
    /// A person's own exchange.
    Theirs,
    /// The work ended unfinished by the loop's own conclusion: the worker's `Could not finish:`
    /// reply, or the same reply again with no work (`why=repeated`). Nothing is typed until
    /// someone else's turn.
    Failed,
}

impl TaskFace {
    /// Every face, in display order.
    pub const ALL: [TaskFace; 4] = [
        TaskFace::Open,
        TaskFace::Landed,
        TaskFace::Theirs,
        TaskFace::Failed,
    ];

    /// The word `status` and `ls` print for it (`task=`).
    #[must_use]
    pub const fn status(self) -> &'static str {
        match self {
            TaskFace::Open => "open",
            TaskFace::Landed => "landed",
            TaskFace::Theirs => "theirs",
            TaskFace::Failed => "failed",
        }
    }
}

/// Why Land still holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LandHeld {
    /// When the land act that began the hold was ledgered, unix ms.
    pub since_ms: Option<i64>,
    /// The nudges (`land-nudge@v1`) typed since that act.
    pub nudges: u32,
    /// Whether a land act's yield was judged and no other turn ran since.
    pub read: bool,
    /// What held the landing at that yield, a bit per [`HoldLeg`].
    pub legs: u8,
    /// The first four words Land's strict reading found open in the agent's list
    /// ([`HoldLeg::Open`]): a description of the list, never what holds it.
    pub open: [Option<OpenWord>; 4],
    /// The repository's work at that yield.
    pub changed: u16,
    pub untracked: u16,
    pub stashes: u16,
    pub unpushed: u16,
    pub unlanded: u16,
    /// Submodule commits no remote of theirs has, and commits parked on another branch the
    /// session used (`RepoWork::sub_unpushed`, `RepoWork::parked`).
    pub sub_unpushed: u16,
    pub parked: u16,
    /// The processes the session's agent started that still ran, unkept, at that yield
    /// ([`HoldLeg::Left`]).
    pub left: u16,
}

impl LandHeld {
    /// Whether `leg` held the landing at the yield read.
    #[must_use]
    pub const fn holds(self, leg: HoldLeg) -> bool {
        self.legs & leg.bit() != 0
    }

    /// The legs that held it, in the order the owner can act on them.
    pub fn legs_held(self) -> impl Iterator<Item = HoldLeg> {
        HoldLeg::ALL.into_iter().filter(move |&leg| self.holds(leg))
    }

    /// The open words, as the strict reading spelled them.
    pub fn open_words(self) -> impl Iterator<Item = &'static str> {
        self.open.into_iter().flatten().map(OpenWord::word)
    }
}

/// What aterm checked at a landing, beside why Land still holds before one (§14.9, slice 9b).
pub use super::land_receipt::{LandReceipt, ReceiptLeg};

/// One leg of a hold ([`LandHeld::legs`]), in the order the owner can act on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HoldLeg {
    /// The reply named a need (`NEEDS YOU:`), in its last words or earlier in the same turn.
    Needs,
    /// The session's repository holds work no remote main has.
    Repo,
    /// The repository probe could not read the repository: what it holds is unknown.
    Unread,
    /// Processes the session's agent started still run, neither ended nor kept by a `keep
    /// <token>` line under its defense (`aterm ctl @self sys left`).
    Left,
    /// The agent's own answer to the land check listed what is left (`land-check@v1`).
    Open,
    /// No `Nothing left:` defense in the reply.
    Defense,
    /// The reply, or the turn it ends, was taller than the rows read.
    Cut,
}

impl HoldLeg {
    /// Every leg, in the order the owner can act on them.
    pub const ALL: [HoldLeg; 7] = [
        HoldLeg::Needs,
        HoldLeg::Repo,
        HoldLeg::Unread,
        HoldLeg::Left,
        HoldLeg::Open,
        HoldLeg::Defense,
        HoldLeg::Cut,
    ];

    /// Its bit in [`LandHeld::legs`].
    #[must_use]
    pub const fn bit(self) -> u8 {
        match self {
            HoldLeg::Needs => 1,
            HoldLeg::Repo => 2,
            HoldLeg::Open => 4,
            HoldLeg::Defense => 8,
            HoldLeg::Cut => 16,
            HoldLeg::Unread => 32,
            HoldLeg::Left => 64,
        }
    }

    /// Its word in `status land_held=`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            HoldLeg::Needs => "needs",
            HoldLeg::Repo => "repo",
            HoldLeg::Open => "open",
            HoldLeg::Defense => "defense",
            HoldLeg::Cut => "cut",
            HoldLeg::Unread => "repo-unread",
            HoldLeg::Left => "left",
        }
    }

    /// The leg of `word` ([`Self::word`]'s inverse); `None` for any other word.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|leg| leg.word() == word)
    }

    /// Its words where they name nothing the reply held, the same in every written form (the
    /// title, the state line, the hover and a row's tip); `None` for the legs whose words name
    /// what the reply, the repository or the processes held (a need, the repository's counts, the
    /// open words, how many processes are left).
    #[must_use]
    pub const fn fixed_words(self) -> Option<&'static str> {
        match self {
            HoldLeg::Unread => Some("a repository that could not be read"),
            HoldLeg::Defense => Some("no \u{201c}Nothing left:\u{201d} defense yet"),
            HoldLeg::Cut => Some("a reply too tall to read whole"),
            HoldLeg::Needs | HoldLeg::Repo | HoldLeg::Open | HoldLeg::Left => None,
        }
    }
}

/// THE OPEN LEG'S WORDS WHERE THE READER NAMES NOTHING IN THE AGENT'S LIST ([`HoldLeg::Open`]):
/// the leg is the agent's own answer to the land check, never the reader's reading.
pub const OPEN_LISTED: &str = "what it listed as left";

/// What a hold's words begin with once its legs are read (`not landed yet: <legs>`).
pub const NOT_LANDED_YET: &str = "not landed yet";

/// Between two facts on one line of words (`landing since 14:02 · 1 nudge`).
pub const FACT_SEPARATOR: &str = " \u{b7} ";

/// A hold's nudges in words: none before the first, then `1 nudge`, `<n> nudges`.
#[must_use]
pub fn nudge_words(nudges: u32) -> Option<String> {
    match nudges {
        0 => None,
        1 => Some("1 nudge".to_owned()),
        n => Some(format!("{n} nudges")),
    }
}

/// A read hold's tip (a worker's menu row, its tab's tip): `not landed yet: <legs>`, then its
/// nudges.
#[must_use]
pub fn held_tip(legs: &str, nudges: u32) -> String {
    match nudge_words(nudges) {
        Some(n) => format!("{NOT_LANDED_YET}: {legs}{FACT_SEPARATOR}{n}"),
        None => format!("{NOT_LANDED_YET}: {legs}"),
    }
}

/// THE LEFT LEG'S WORDS ([`HoldLeg::Left`]), the same in every written form: how many processes
/// the session's agent started that still run, neither ended nor kept (`1 process left running`,
/// `3 processes left running`).
#[must_use]
pub fn left_words(left: u16) -> String {
    match left {
        1 => "1 process left running".to_owned(),
        n => format!("{n} processes left running"),
    }
}

/// WHY ATERM STOPPED TYPING INTO A TAB (§14.3 of the Autopilot design): the arm of ruling F's
/// hold that holds it ([`crate::supervise::policy::turn_end::Replies::held_by`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HeldBy {
    /// The newest reply came again with no work (or a read wall's tries are spent at it).
    SameReply,
    /// [`crate::supervise::policy::turn_end::NO_WORK_ACTS`] no-work answers in a row, whatever
    /// their wording.
    NoWork,
}

impl HeldBy {
    /// Both arms.
    pub const ALL: [HeldBy; 2] = [HeldBy::SameReply, HeldBy::NoWork];

    /// Its word in `status held=` and the journal's `EXCEPTION … held=`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            HeldBy::SameReply => "same-reply",
            HeldBy::NoWork => "no-work",
        }
    }

    /// The arm of `word` ([`Self::word`]'s inverse); `None` for `-` and any other word.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|arm| arm.word() == word)
    }

    /// What held it, in the written forms (`the same reply twice, no work`).
    #[must_use]
    pub fn why(self) -> String {
        match self {
            HeldBy::SameReply => "the same reply twice, no work".to_owned(),
            HeldBy::NoWork => format!(
                "{} replies in a row, no work",
                crate::supervise::policy::turn_end::NO_WORK_ACTS
            ),
        }
    }

    /// What held it as a screen reader says it (`the same reply twice, with no work`).
    #[must_use]
    pub fn spoken_why(self) -> String {
        match self {
            HeldBy::SameReply => "the same reply twice, with no work".to_owned(),
            HeldBy::NoWork => format!(
                "{} replies in a row, with no work",
                crate::supervise::policy::turn_end::NO_WORK_ACTS
            ),
        }
    }

    /// How its program answered aterm's acts, as the hold's exception says it after `answered`
    /// (`twice with the same reply and no work`; `Exception::repeats`).
    #[must_use]
    pub fn answered(self) -> String {
        match self {
            HeldBy::SameReply => "twice with the same reply and no work".to_owned(),
            HeldBy::NoWork => format!(
                "{} times in a row with no work",
                crate::supervise::policy::turn_end::NO_WORK_ACTS
            ),
        }
    }
}

/// What every surface of a stopped tab begins with ([`stop_words`]), read without regard to case.
pub const STOPPED_TYPING: &str = "stopped typing";

/// What starts a stopped tab's typing again, in the written forms: the person's turn or the
/// agent's work. A switch to Cruise does not (`act_past_walls` answers a repeat with nothing),
/// Off's park clears the hold, and a program that stopped its own typing under its proven claim
/// (codex-alab, §14.3.2) restarts by its own loop's rule, of which aterm knows these two.
pub const TYPES_AGAIN: &str = "types again at your turn or once the agent works";

/// [`TYPES_AGAIN`] where the person's switch to Land would ask once more over the stop (v6
/// §14.17 item 3, [`SWITCH_ASKED`]): under Cruise, over the agent's own reply, which no read wall
/// said and no ask was spent on (the hold's exception says it,
/// `Exception::lifted_by_switch`). Under Land there is no switch to make.
pub const TYPES_AGAIN_OR_SWITCH: &str =
    "types again at your turn, your switch to Land, or once the agent works";

/// THE PERSON'S SWITCH TO LAND OVER A STOPPED TAB, AS THE TAB'S STATE LINE SAYS IT (v6 §14.17
/// item 3), from Land's one ask until its answer is read: the switch counted as their turn, so
/// Land typed `land@v1` once more, and the same answer holds again with no second prompt.
pub const SWITCH_ASKED: &str = "Land asked once more because you switched to Land. It holds again \
                                if the answer is the same.";

/// THE FIVE PLACES A STOP IS SAID ([`stop_words`]), as a hold's words are said in five
/// (`aterm-gui`'s `HoldForm`, which maps onto these one for one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopForm {
    /// The chip's title, after `autopilot: <mode> — `: one part of it, its own cuts `;`.
    Title,
    /// Settings ▸ Harness's state line: its facts ` · ` apart.
    Line,
    /// The tab's hover and the Overview (`driven by aterm (land: stopped typing; …)`).
    Hover,
    /// A worker's row in the menu bar.
    Tip,
    /// The chip's accessibility description's lead.
    Spoken,
}

/// WHO DRIVES A TAB UNDER AN AUTOPILOT, AS ITS HOVER AND THE OVERVIEW SAY IT: aterm's harness,
/// the head of [`driver_line`] (`driven by aterm (land: …)`). One source for the window that
/// writes the line and the lab that reads it.
pub const ATERM_DRIVES: &str = "driven by aterm";

/// The same line's head on a tab whose codex-alab drives itself inside its own process, its own
/// claim proven (`codex-alab autopilot (land: …)`).
pub const CODEX_ALAB_DRIVES: &str = "codex-alab autopilot";

/// THE DRIVER LINE: `head` ([`ATERM_DRIVES`] or [`CODEX_ALAB_DRIVES`]) and the Autopilot's words
/// in brackets.
#[must_use]
pub fn driver_line(head: &str, autopilot: &str) -> String {
    format!("{head} ({autopilot})")
}

/// Whether `line` is a [`driver_line`] under either head.
#[must_use]
pub fn is_driver_line(line: &str) -> bool {
    [ATERM_DRIVES, CODEX_ALAB_DRIVES].into_iter().any(|head| {
        line.strip_prefix(head)
            .is_some_and(|rest| rest.starts_with(" ("))
    })
}

/// A STOPPED AUTOPILOT, IN WORDS (§14.3): ONE function for every surface that says the stop —
/// the chip's title and description, the state line, the hover and the Overview, a worker's tip —
/// and for the lab that reads them. `switch`: the person's switch to Land would ask once more
/// over it ([`TYPES_AGAIN_OR_SWITCH`]), else [`TYPES_AGAIN`].
#[must_use]
pub fn stop_words(held: HeldBy, form: StopForm, switch: bool) -> String {
    let again = if switch {
        TYPES_AGAIN_OR_SWITCH
    } else {
        TYPES_AGAIN
    };
    match form {
        StopForm::Title => format!("{STOPPED_TYPING}: {}; {again}", held.why()),
        StopForm::Line => format!("{STOPPED_TYPING}: {}{FACT_SEPARATOR}{again}", held.why()),
        StopForm::Hover => STOPPED_TYPING.to_owned(),
        StopForm::Tip => format!("{STOPPED_TYPING}: {}", held.why()),
        StopForm::Spoken => format!("Stopped typing: {}. It {again}.", held.spoken_why()),
    }
}

/// One word Land's strict reading found open, as an index into its word tables.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpenWord(std::num::NonZeroU16);

impl OpenWord {
    /// The index of `word`, a spelling the strict reading names.
    #[must_use]
    pub fn of(word: &str) -> Option<Self> {
        crate::supervise::policy::turn_end::open_word_index(word).map(Self)
    }

    /// The word, as the strict reading spelled it.
    #[must_use]
    pub fn word(self) -> &'static str {
        crate::supervise::policy::turn_end::open_word_at(self.0).unwrap_or("open work")
    }
}

impl std::fmt::Debug for OpenWord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.word())
    }
}

#[cfg(test)]
#[path = "acts_tests.rs"]
mod tests;
