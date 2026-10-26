// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Harness act and landing projections shared across the wire.

use crate::supervise::approvals::Outcome;
use crate::supervise::policy::approval::{
    QuestionPriority, RULE_PLAN, RULE_PROSE_LONG_TERM, RULE_PROSE_SHORT_TERM,
};
use crate::supervise::policy::question::{RULE_ANSWER_RECOMMENDED, is_question_rule};
use crate::supervise::policy::turn_end::{
    RULE_ANSWER, RULE_API_BACK, RULE_API_CUTOFF, RULE_API_RETRY, RULE_COMPACT, RULE_CONTINUE,
    RULE_DONE_CHECK, RULE_HOST_TURN, RULE_LAND, RULE_LAND_ANSWER, RULE_LAND_NUDGE,
    RULE_LIMIT_RESUME, RULE_LOGIN_BACK, RULE_OPEN_ITEMS, RULE_SUGGESTION, RULE_UNCOMMITTED,
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
}

impl Act {
    /// Every act, in display order.
    pub const ALL: [Act; 7] = [
        Act::Approved,
        Act::Declined,
        Act::Answered,
        Act::Continued,
        Act::Resumed,
        Act::Landing,
        Act::Nudged,
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
            Act::Landing | Act::Nudged => "\u{2913}",
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
        || QuestionPriority::ALL
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
    ) {
        Some((Act::Continued, Counter::Continued))
    } else if is_prose_answer(rule) {
        Some((Act::Answered, Counter::Answered))
    } else if rule == RULE_LAND {
        Some((Act::Landing, Counter::LandPrompts))
    } else if rule == RULE_LAND_NUDGE {
        Some((Act::Nudged, Counter::LandPrompts))
    } else if matches!(
        rule,
        RULE_LIMIT_RESUME | RULE_API_RETRY | RULE_API_BACK | RULE_API_CUTOFF | RULE_LOGIN_BACK
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
            Act::Landing | Act::Nudged => "dispatch",
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
        RULE_APPROVE_ALL => "approve",
        RULE_ANSWER_RECOMMENDED
        | RULE_LAND_ANSWER
        | RULE_ANSWER
        | RULE_PROSE_LONG_TERM
        | RULE_PROSE_SHORT_TERM => "answer",
        RULE_CONTINUE | RULE_POLICY_CONTINUE | RULE_LIMIT_RESUME | RULE_DONE_CHECK
        | RULE_API_RETRY | RULE_API_CUTOFF | RULE_COMPACT => "continue",
        RULE_UPGRADE => "relaunch",
        RULE_HOST_TURN | RULE_LAND | RULE_LAND_NUDGE => "dispatch",
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
}

impl TaskFace {
    /// Every face, in display order.
    pub const ALL: [TaskFace; 3] = [TaskFace::Open, TaskFace::Landed, TaskFace::Theirs];

    /// The word `status` and `ls` print for it (`task=`).
    #[must_use]
    pub const fn status(self) -> &'static str {
        match self {
            TaskFace::Open => "open",
            TaskFace::Landed => "landed",
            TaskFace::Theirs => "theirs",
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
    /// The first four words Land's strict reading found open ([`HoldLeg::Open`]).
    pub open: [Option<OpenWord>; 4],
    /// The repository's work at that yield.
    pub changed: u16,
    pub untracked: u16,
    pub stashes: u16,
    pub unpushed: u16,
    pub unlanded: u16,
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

/// One leg of a hold ([`LandHeld::legs`]), in the order the owner can act on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HoldLeg {
    /// The reply named a need (`NEEDS YOU:`), in its last words or earlier in the same turn.
    Needs,
    /// The session's repository holds work no remote main has.
    Repo,
    /// The repository probe could not read the repository: what it holds is unknown.
    Unread,
    /// Land's strict reading found open work in the reply.
    Open,
    /// No `Nothing left:` defense in the reply.
    Defense,
    /// The reply, or the turn it ends, was taller than the rows read.
    Cut,
}

impl HoldLeg {
    /// Every leg, in the order the owner can act on them.
    pub const ALL: [HoldLeg; 6] = [
        HoldLeg::Needs,
        HoldLeg::Repo,
        HoldLeg::Unread,
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
        }
    }

    /// Its words where they name nothing the reply held, the same in every written form (the
    /// title, the state line, the hover and a row's tip); `None` for the legs whose words name
    /// what the reply or the repository held (a need, the repository's counts, the open words).
    #[must_use]
    pub const fn fixed_words(self) -> Option<&'static str> {
        match self {
            HoldLeg::Unread => Some("a repository that could not be read"),
            HoldLeg::Defense => Some("no \u{201c}Nothing left:\u{201d} defense yet"),
            HoldLeg::Cut => Some("a reply too tall to read whole"),
            HoldLeg::Needs | HoldLeg::Repo | HoldLeg::Open => None,
        }
    }
}

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
    /// (`twice with the same reply and no work`; `Exception::repeats`, `Exception::repeats_land`).
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

/// What starts a stopped tab's typing again, in the written forms: a mode switch does not
/// (`act_past_walls` answers a repeat with nothing in either mode), Off's park does.
pub const TYPES_AGAIN: &str = "types again at your turn or once the agent works";

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

/// A STOPPED AUTOPILOT, IN WORDS (§14.3): ONE function for every surface that says the stop —
/// the chip's title and description, the state line, the hover and the Overview, a worker's tip —
/// and for the lab that reads them.
#[must_use]
pub fn stop_words(held: HeldBy, form: StopForm) -> String {
    match form {
        StopForm::Title => format!("{STOPPED_TYPING}: {}; {TYPES_AGAIN}", held.why()),
        StopForm::Line => format!(
            "{STOPPED_TYPING}: {}{FACT_SEPARATOR}{TYPES_AGAIN}",
            held.why()
        ),
        StopForm::Hover => STOPPED_TYPING.to_owned(),
        StopForm::Tip => format!("{STOPPED_TYPING}: {}", held.why()),
        StopForm::Spoken => format!(
            "Stopped typing: {}. It types again at your turn, or once the agent works.",
            held.spoken_why()
        ),
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
mod tests {
    use super::*;

    /// `status held=` and the journal's ` held=` read back to the arm they print, `-` to none, and
    /// every form of the stop leads with the one stop word.
    #[test]
    fn a_holds_arm_round_trips_its_word_and_every_stop_form_leads_with_the_stop() {
        for held in HeldBy::ALL {
            assert_eq!(HeldBy::of_word(held.word()), Some(held));
            for form in [
                StopForm::Title,
                StopForm::Line,
                StopForm::Hover,
                StopForm::Tip,
            ] {
                assert!(
                    stop_words(held, form).starts_with(STOPPED_TYPING),
                    "{held:?} {form:?}"
                );
            }
            let spoken = stop_words(held, StopForm::Spoken);
            assert!(spoken.ends_with('.'), "{spoken}");
        }
        assert_eq!(HeldBy::of_word("-"), None);
        assert_ne!(
            stop_words(HeldBy::SameReply, StopForm::Line),
            stop_words(HeldBy::NoWork, StopForm::Line),
            "the arms say different whys"
        );
    }

    #[test]
    fn every_act_has_its_face_and_words_with_no_glyph() {
        let mut faces = std::collections::BTreeSet::new();
        let mut said = std::collections::BTreeSet::new();
        assert_eq!(Act::Approved.spoken(), "approved");
        assert_eq!(Act::Landing.spoken(), "started landing");
        for act in Act::ALL {
            assert!(faces.insert((act.glyph(), act.word())), "{act:?}");
            assert!(said.insert(act.spoken()), "{act:?}: {}", act.spoken());
            if act != Act::Landing {
                assert_eq!(act.spoken(), act.word(), "{act:?}");
            }
            assert!(
                act.spoken()
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == ' '),
                "{act:?}: {}",
                act.spoken()
            );
        }
    }

    use crate::supervise::policy::approval::{
        RULE_ALLOW_ONCE, RULE_ANSWER_LONG_TERM, RULE_ANSWER_SHORT_TERM, RULE_LIMIT_WAIT, RULE_PLAN,
        RULE_PROSE_LONG_TERM, RULE_PROSE_SHORT_TERM, RULE_READ_ONLY, RULE_TRUST_DIALOG,
    };
    use crate::supervise::policy::question::RULE_ANSWER_RECOMMENDED;
    use crate::supervise::policy::turn_end::{
        RULE_ACCOUNT, RULE_ANSWER, RULE_CONSENT, RULE_HOST_TURN, RULE_LOGIN, RULE_MEMORY_RESTART,
        RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE, RULE_USAGE_PROBE,
    };

    fn family(act: Act) -> Counter {
        match act {
            Act::Approved => Counter::Approved,
            Act::Declined => Counter::Declined,
            Act::Answered => Counter::Answered,
            Act::Continued => Counter::Continued,
            Act::Resumed => Counter::Resumed,
            Act::Landing | Act::Nudged => Counter::LandPrompts,
        }
    }

    #[test]
    fn the_sort_is_the_stats_tables() {
        let lit = |act, counter| Sorted {
            act: Some(act),
            counter: Some(counter),
        };
        let approved = Outcome::Approved;
        let typed = Outcome::Typed;
        for rule in [
            RULE_ALLOW_ONCE,
            RULE_READ_ONLY,
            RULE_PLAN,
            RULE_TRUST_DIALOG,
        ] {
            assert_eq!(
                sort(rule, approved, "ls"),
                lit(Act::Approved, Counter::Approved),
                "{rule}"
            );
        }
        assert_eq!(
            sort(RULE_LIMIT_WAIT, approved, ""),
            lit(Act::Approved, Counter::Approved)
        );
        let keys = [
            RULE_ANSWER_RECOMMENDED,
            RULE_ANSWER_LONG_TERM,
            RULE_ANSWER_SHORT_TERM,
            RULE_LAND_ANSWER,
        ];
        for rule in keys {
            assert_eq!(
                sort(rule, approved, ""),
                Sorted {
                    act: Some(Act::Answered),
                    counter: None,
                },
                "{rule}: a dialog's key lights and counts nothing"
            );
        }
        assert_eq!(
            sort(RULE_READ_ONLY, Outcome::Declined, "rm -rf /"),
            lit(Act::Declined, Counter::Declined)
        );
        for rule in [
            RULE_CONTINUE,
            RULE_POLICY_CONTINUE,
            RULE_SUGGESTION,
            RULE_DONE_CHECK,
            RULE_OPEN_ITEMS,
            RULE_UNCOMMITTED,
        ] {
            assert_eq!(
                sort(rule, typed, "keep going"),
                lit(Act::Continued, Counter::Continued),
                "{rule}"
            );
        }
        for rule in [
            RULE_ANSWER,
            RULE_PROSE_LONG_TERM,
            RULE_PROSE_SHORT_TERM,
            RULE_LAND_ANSWER,
        ] {
            assert_eq!(
                sort(rule, typed, "the long-term one"),
                lit(Act::Answered, Counter::Answered),
                "{rule}"
            );
        }
        assert_eq!(
            sort(RULE_LAND, typed, "land it"),
            lit(Act::Landing, Counter::LandPrompts)
        );
        assert_eq!(
            sort(RULE_LAND_NUDGE, typed, "still landing?"),
            lit(Act::Nudged, Counter::LandPrompts)
        );
        for rule in [
            RULE_LIMIT_RESUME,
            RULE_API_RETRY,
            RULE_API_BACK,
            RULE_API_CUTOFF,
            RULE_LOGIN_BACK,
        ] {
            assert_eq!(
                sort(rule, typed, "continue"),
                lit(Act::Resumed, Counter::Resumed),
                "{rule}"
            );
        }
        assert_eq!(
            sort(RULE_COMPACT, typed, "/compact"),
            Sorted::default(),
            "/compact lights and counts nothing"
        );
        assert_eq!(
            sort(RULE_COMPACT, typed, "continue where you left off"),
            lit(Act::Resumed, Counter::Resumed),
            "the continuation resumes, for both"
        );
        for rule in [
            RULE_LOGIN,
            RULE_CONSENT,
            RULE_MODEL_FALLBACK,
            RULE_MODEL_RESTORE,
            RULE_ACCOUNT,
            RULE_MEMORY_RESTART,
            "model-wind-down@v1",
            RULE_HOST_TURN,
            RULE_USAGE_PROBE,
            "a-rule-this-build-never-wrote@v9",
        ] {
            assert_eq!(sort(rule, typed, "/model"), Sorted::default(), "{rule}");
        }
        let every_rule = [
            RULE_ALLOW_ONCE,
            RULE_ANSWER_RECOMMENDED,
            RULE_LAND_ANSWER,
            RULE_CONTINUE,
            RULE_LAND,
            RULE_LIMIT_RESUME,
            RULE_COMPACT,
            RULE_LOGIN,
        ];
        for outcome in [
            Outcome::Skipped,
            Outcome::Deferred,
            Outcome::Waiting,
            Outcome::Escalated,
        ] {
            for rule in every_rule {
                assert_eq!(
                    sort(rule, outcome, "continue"),
                    Sorted::default(),
                    "{rule} {outcome:?}"
                );
            }
        }
        let mut parted = Vec::new();
        for outcome in Outcome::ALL {
            for rule in every_rule.into_iter().chain(keys) {
                for command in ["/compact", "continue"] {
                    let sorted = sort(rule, outcome, command);
                    if sorted.act.map(family) != sorted.counter {
                        parted.push((rule, outcome));
                    }
                }
            }
        }
        parted.sort_unstable_by_key(|(rule, outcome)| (*rule, outcome.word()));
        parted.dedup();
        let mut want: Vec<_> = keys.iter().map(|rule| (*rule, approved)).collect();
        want.sort_unstable_by_key(|(rule, outcome)| (*rule, outcome.word()));
        assert_eq!(parted, want);
    }

    #[test]
    fn every_face_prints_its_own_status_word() {
        let words: std::collections::BTreeSet<_> =
            TaskFace::ALL.iter().map(|f| f.status()).collect();
        assert_eq!(
            words.into_iter().collect::<Vec<_>>(),
            ["landed", "open", "theirs"]
        );
    }

    #[test]
    fn each_act_is_one_log_word() {
        use crate::supervise::policy::turn_end::RULE_LAND_NUDGE;
        let cases = [
            (
                "read-only@v3",
                Outcome::Approved,
                "1",
                Some("act word=approve rule=read-only.v3"),
            ),
            (
                "answer-recommended@v1",
                Outcome::Approved,
                "1",
                Some("act word=answer rule=answer-recommended.v1"),
            ),
            (
                "write@v1",
                Outcome::Declined,
                "2",
                Some("act word=deny rule=write.v1"),
            ),
            (
                RULE_CONTINUE,
                Outcome::Typed,
                "go on",
                Some("act word=continue rule=continue.v1"),
            ),
            (
                RULE_LIMIT_RESUME,
                Outcome::Typed,
                "go on",
                Some("act word=continue rule=usage-resume.v1"),
            ),
            (
                RULE_LAND,
                Outcome::Typed,
                "land",
                Some("act word=dispatch rule=land.v1"),
            ),
            (
                RULE_LAND_NUDGE,
                Outcome::Typed,
                "land",
                Some("act word=dispatch rule=land-nudge.v1"),
            ),
            ("-", Outcome::Escalated, "", Some("act word=escalate")),
            ("write@v1", Outcome::Skipped, "", None),
            ("write@v1", Outcome::Deferred, "", None),
            (RULE_COMPACT, Outcome::Typed, "/compact", None),
        ];
        for (rule, outcome, command, want) in cases {
            assert_eq!(
                log_mark(rule, outcome, command).as_deref(),
                want,
                "{rule} {outcome:?}"
            );
        }
        let long = format!("{}@v1 x", "r".repeat(60));
        let mark = log_mark(&long, Outcome::Escalated, "").expect("an escalation");
        assert_eq!(mark, format!("act word=escalate rule={}", "r".repeat(40)));
    }

    /// The harness labels only its own moments, in closed words, and every label fits the `mark`
    /// verb's 32 graphemes and 40 bytes as one `text=` word.
    #[test]
    fn the_harness_labels_its_own_moments_in_closed_words() {
        let cases = [
            ("relaunch", "relaunched", "2.1.0", Some("restarting")),
            ("relaunch", "adopted", "-", Some("restarting")),
            (
                "carry-on-after-update",
                "done:fresh",
                "2.1.0",
                Some("restarting"),
            ),
            (
                "self-claimed-relaunch",
                "relaunched",
                "",
                Some("restarting"),
            ),
            ("restart:account", "relaunched", "2.1.0", Some("restarting")),
            (
                "upgrade",
                "relaunched",
                "2.1.304",
                Some("upgrading to 2.1.304"),
            ),
            (
                "upgrade",
                "relaunched",
                "2.2.0(npm)",
                Some("upgrading to 2.2.0"),
            ),
            ("upgrade", "relaunched", "(npm)", Some("upgrading")),
            (
                "upgrade",
                "done:unconfirmed",
                "0.164.0-dev.12521+g3d3eb12d8",
                Some("upgrading to 0.164.0-dev.12521"),
            ),
            (
                "upgrade-at-break",
                "adopted",
                "2.1.304",
                Some("upgrading to 2.1.304"),
            ),
            ("upgrade", "relaunched", "-", Some("upgrading")),
            ("upgrade", "relaunched", "2.1.304 ; rm", Some("upgrading")),
            (
                "upgrade",
                "relaunched",
                "1.2.3-a-very-long-pre-release",
                Some("upgrading"),
            ),
            ("relaunch", "wait:held", "2.1.0", None),
            ("carry-on", "continued", "2.1.0", None),
            ("upgrade-at-break", "announced:1", "2.1.304", None),
            ("carry-restart", "terminated", "2.1.0", None),
            ("restart:account", "exited", "2.1.0", None),
        ];
        for (row, step, to, want) in cases {
            assert_eq!(
                relaunch_label(row, step, to).as_deref(),
                want,
                "{row} {step} {to}"
            );
        }
        assert_eq!(
            loop_label(RULE_LIMIT_RESUME, Outcome::Typed),
            Some(LIMIT_RESUME_LABEL)
        );
        for (rule, outcome) in [
            (RULE_LIMIT_RESUME, Outcome::Waiting),
            (RULE_LIMIT_RESUME, Outcome::Skipped),
            (RULE_CONTINUE, Outcome::Typed),
            (RULE_API_RETRY, Outcome::Typed),
            (RULE_LOGIN_BACK, Outcome::Typed),
            (RULE_LAND, Outcome::Typed),
        ] {
            assert_eq!(loop_label(rule, outcome), None, "{rule} {outcome:?}");
        }
        assert_eq!(
            label_mark(LIMIT_RESUME_LABEL),
            "label text=resuming%20after%20limit ttl=30"
        );
        for words in [
            RESTARTING_LABEL,
            DISK_RESUME_LABEL,
            DISK_FREE_LABEL,
            LIMIT_RESUME_LABEL,
            "upgrading to 0.164.0-dev.12521",
            "upgrading to 1234567890123456789",
        ] {
            assert!(
                words.len() <= 40 && words.chars().count() <= 32,
                "{words}: within the mark verb's label bound"
            );
            assert!(!label_mark(words).contains("  "), "{words}");
            assert_eq!(label_mark(words).split(' ').count(), 3, "{words}");
        }
    }

    #[test]
    fn a_self_claims_acts_are_the_logs_act_words() {
        let cases = [
            ("approve-all@v1", Some("approve")),
            ("answer-recommended@v1", Some("answer")),
            ("continue@v1", Some("continue")),
            ("policy-continue@v1", Some("continue")),
            ("usage-resume@v1", Some("continue")),
            ("upgrade@v1", Some("relaunch")),
            ("harness-turn@v1", Some("dispatch")),
            ("continue-done-check@v1", Some("continue")),
            ("land@v1", Some("dispatch")),
            ("land-nudge@v1", Some("dispatch")),
            ("land-answer@v1", Some("answer")),
            ("landed@v1", Some("landed")),
            ("answer@v1", Some("answer")),
            ("prose-long-term@v1", Some("answer")),
            ("prose-short-term@v1", Some("answer")),
            ("api-retry@v1", Some("continue")),
            ("api-cutoff@v1", Some("continue")),
            ("context-compact@v1", Some("continue")),
            ("finished@v1", None),
            ("left-to-person@v1", None),
            ("approve-all@v2", None),
            ("approve-all", None),
            ("read-only@v3", None),
            ("continue", None),
            ("landed@v2", None),
            ("land", None),
            ("continue-open-items@v1", None),
            ("continue-uncommitted@v1", None),
        ];
        for (act, want) in cases {
            let got = self_claim_act(act);
            assert_eq!(got.as_ref().map(|g| g.0), want, "{act}");
            if let Some((word, rule)) = got {
                assert_eq!(rule, act.replace('@', "."), "{act}");
                assert!(
                    aterm_slog::Fields::new()
                        .word(aterm_slog::Key::Word, word)
                        .is_ok(),
                    "{act}: the log has no word {word:?}"
                );
            }
        }
        let unmarked = cases.iter().filter(|c| c.1.is_none()).count();
        assert_eq!(unmarked, 10, "the acts that write no mark, counted");
        assert!(
            aterm_slog::Fields::new()
                .word(aterm_slog::Key::Word, "landing")
                .is_err(),
            "the vocabulary refuses a word it does not hold"
        );
        for rule in [
            RULE_CONTINUE,
            RULE_POLICY_CONTINUE,
            RULE_LIMIT_RESUME,
            RULE_DONE_CHECK,
            RULE_LAND,
            RULE_LAND_NUDGE,
            RULE_LAND_ANSWER,
            RULE_ANSWER,
            RULE_PROSE_LONG_TERM,
            RULE_PROSE_SHORT_TERM,
            RULE_API_RETRY,
            RULE_API_CUTOFF,
            RULE_COMPACT,
        ] {
            let loops = log_mark(rule, Outcome::Typed, "words").expect("a typed act");
            let (word, _) = self_claim_act(rule).expect("a self-claim act");
            assert!(
                loops.starts_with(&format!("act word={word} ")),
                "{rule}: {loops}"
            );
        }
    }

    #[test]
    fn a_self_claims_act_types_a_turn_only_where_it_starts_one() {
        let cases = [
            ("continue@v1", true),
            ("continue-done-check@v1", true),
            ("land@v1", true),
            ("land-nudge@v1", true),
            ("land-answer@v1", true),
            ("answer@v1", true),
            ("prose-long-term@v1", true),
            ("prose-short-term@v1", true),
            ("api-retry@v1", true),
            ("api-back@v1", true),
            ("api-cutoff@v1", true),
            ("usage-resume@v1", true),
            ("context-compact@v1", true),
            ("harness-turn@v1", true),
            ("plan-approve@v2", true),
            ("approve-all@v1", false),
            ("answer-recommended@v1", false),
            ("answer-long-term@v1", false),
            ("answer-short-term@v1", false),
            ("mail-report@v1", false),
            ("auth-login@v1", false),
            ("landed@v1", false),
            ("left-to-person@v1", false),
            ("upgrade@v1", false),
            ("finished@v1", false),
            ("continue", false),
            ("land@v2", false),
        ];
        let wrong: Vec<_> = cases
            .iter()
            .filter(|(act, want)| self_claim_types_turn(act) != *want)
            .collect();
        assert!(wrong.is_empty(), "{wrong:?}");
        let worded = |act| self_claim_act(act).is_some();
        assert!(worded("approve-all@v1") && worded("harness-turn@v1"));
        assert!(!worded("plan-approve@v2") && !worded("mail-report@v1"));
    }

    #[test]
    fn a_hosted_relaunch_row_that_moved_the_session_is_a_relaunch_act() {
        let cases = [
            ("relaunch", "relaunched", Some("relaunch")),
            ("relaunch", "adopted", Some("relaunch")),
            (
                "self-claimed-relaunch",
                "adopted",
                Some("self-claimed-relaunch"),
            ),
            (
                "relaunch-after-host-ended",
                "relaunched",
                Some("relaunch-after-host-ended"),
            ),
            ("carry-restart", "terminated", Some("carry-restart")),
            ("restart:account", "exited", Some("restart:account")),
            ("upgrade", "announced:1", Some("upgrade")),
            (
                "upgrade-at-break",
                "released:void",
                Some("upgrade-at-break"),
            ),
            ("carry-on", "continued", Some("carry-on")),
            (
                "carry-on-after-update",
                "done:fresh",
                Some("carry-on-after-update"),
            ),
            ("relaunch", "wait:held", None),
            ("relaunch", "failed:no-resume", None),
            ("carry-restart", "refused:not-a-shell-job", None),
            ("upgrade", "current", None),
            ("upgrade", "gave-up", None),
            ("upgrade", "would-restart", None),
            ("restart:account", "busy:typing", None),
        ];
        for (row, step, want) in cases {
            assert_eq!(relaunch_act(row, step).as_deref(), want, "{row} {step}");
        }
        assert_eq!(RELAUNCH_WORD, "relaunch");
    }

    #[test]
    fn only_a_step_that_brought_a_new_agent_up_started_one() {
        use super::super::upgrade_drive::{moved, started};
        let cases = [
            ("relaunched", true),
            ("adopted", true),
            ("done:fresh", true),
            ("done:unconfirmed", true),
            ("announced:1", false),
            ("continued", false),
            ("continued:restore", false),
            ("continued:check", false),
            ("released:void", false),
            ("terminated", false),
            ("exited", false),
            ("wait:held", false),
            ("done", false),
            ("done:resumed", false),
            ("done:model-unconfirmed", false),
            ("failed:no-resume", false),
            ("gave-up", false),
        ];
        for (step, want) in cases {
            assert_eq!(started(step), want, "{step}");
            assert!(
                !started(step) || moved(step),
                "{step} started but did not move"
            );
        }
    }
}
