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
    /// Land's strict reading found open work in the reply.
    Open,
    /// No `Nothing left:` defense in the reply.
    Defense,
    /// The reply, or the turn it ends, was taller than the rows read.
    Cut,
}

impl HoldLeg {
    /// Every leg, in the order the owner can act on them.
    pub const ALL: [HoldLeg; 5] = [
        HoldLeg::Needs,
        HoldLeg::Repo,
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
        }
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
