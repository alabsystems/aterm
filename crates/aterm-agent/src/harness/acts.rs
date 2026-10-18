// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE HARNESS JUST DID, AND HOW A LANDING ENDED, as the window shows
//! them (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9): the words both
//! ends of the wire share.
//!
//! * [`Act`] — one act of the harness's that the Autopilot chip may light for
//!   a moment (§9.2.1's seven), with its glyph, its word and its words for a
//!   screen reader. Told by the loop's single ledger writer through
//!   `IdleHost::act_taken` ([`crate::supervise::IdleHost::act_taken`]).
//! * [`Counter`] — the counters of Settings ▸ Harness's WHAT THE HARNESS DID
//!   ([`super::stats::HarnessStats`]) a ledger row adds to. An act and its
//!   counter differ where the stats table already makes them differ: a
//!   question dialog's key lights `Answered` and counts nothing (the
//!   journal's `chose` counts the dialog, once).
//! * [`TaskFace`] — the task's face under Land (§9.4.2): open, landed, done,
//!   spent or theirs, and the word `status task=` prints for it. Told by the
//!   loop through `IdleHost::task_face`
//!   ([`crate::supervise::IdleHost::task_face`]).
//!
//! * [`sort`] — THE ONE SORT of a ledger row (its rule, its outcome and the
//!   command it typed) into the act it lights and the counter it adds to
//!   (§9.2.3, item 1). The loop's ledger writer tells the window the act
//!   (`Session::ledger_row`), and the page's fold counts the counter
//!   ([`super::stats::fold_ledger_line`]): one function says both, so the
//!   chip and the page never disagree about what an act is.
//!
//! Nothing here reads a ledger, a screen or a clock.

use crate::supervise::approvals::Outcome;
use crate::supervise::policy::approval::QuestionPriority;
use crate::supervise::policy::question::is_question_rule;
// A rule id is never reworded (a ledger an earlier build wrote reads the
// same), so the sort is the policy's own consts.
use crate::supervise::policy::turn_end::{
    RULE_API_BACK, RULE_API_CUTOFF, RULE_API_RETRY, RULE_COMPACT, RULE_CONTINUE, RULE_DONE_CHECK,
    RULE_LAND, RULE_LAND_ANSWER, RULE_LAND_NUDGE, RULE_LIMIT_RESUME, RULE_OPEN_ITEMS,
    RULE_SUGGESTION,
};

/// One act of the harness's, as the Autopilot chip names it for a moment
/// (§9.2.1): the seven the page's own act families hold (approved, declined,
/// answered, continued, resumed, landed — Land's instruction and its nudge
/// apart). Nothing else lights the chip: not a `skipped`, `deferred`,
/// `waiting` or `escalated` row, and not the `typed` rows the page counts
/// only by rule (`/compact`, `/model`, a login, a consent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Act {
    /// A box approved (`approved` under a box's rule).
    Approved,
    /// A box declined with a reason (`declined`).
    Declined,
    /// A question answered: a dialog's keys (one label for the whole dialog)
    /// or a prose answer typed (`answer@v1`, `prose-*@v1`, `land-answer@v1`).
    Answered,
    /// The work kept moving (`continue@v1`, `continue-suggestion@v1`,
    /// `continue-done-check@v1`, `continue-open-items@v1`).
    Continued,
    /// Carried past a wall: a usage reset, an API error, a cut reply, a full
    /// context's continuation (not its `/compact`).
    Resumed,
    /// Land's instruction typed (`land@v1`).
    Landing,
    /// A land nudge typed (`land-nudge@v1`).
    Nudged,
}

impl Act {
    /// Every act, in §9.2.1's order.
    pub const ALL: [Act; 7] = [
        Act::Approved,
        Act::Declined,
        Act::Answered,
        Act::Continued,
        Act::Resumed,
        Act::Landing,
        Act::Nudged,
    ];

    /// The act's glyph, drawn before its word: `✓`, `✗`, `?`, `↻`, `⧗` (the
    /// footer's own wall mark) and `⤓` for both of Land's.
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

    /// The act in words a screen reader says, no glyph: letters and spaces
    /// only, in §9.2.1's form (`Last: approved at 14:05.`) — the label's own
    /// word, but for Land's instruction, whose `landing` would read as a
    /// landing still to come at that time: `started landing`.
    #[must_use]
    pub const fn spoken(self) -> &'static str {
        match self {
            Act::Landing => "started landing",
            act => act.word(),
        }
    }
}

/// A counter of WHAT THE HARNESS DID ([`super::stats::HarnessStats`]) one
/// ledger row adds to; a row with none counts only by its rule.
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
    /// [`super::stats::HarnessStats::land_prompts`]: Land's instruction and
    /// its nudges typed (`land@v1`, `land-nudge@v1`) — land PROMPTS, never
    /// landings: the chip's `⤓ landing` and `⤓ nudged` light the same rows.
    LandPrompts,
}

/// What one ledger row is ([`sort`]): the act it lights on the Autopilot
/// chip, and the counter of WHAT THE HARNESS DID it adds to. Apart, because
/// they differ where the stats table already makes them differ: a question
/// dialog's key lights [`Act::Answered`] and counts nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Sorted {
    /// The act the chip lights for a moment; `None`: the row lights nothing.
    pub act: Option<Act>,
    /// The counter the page adds the row to; `None`: only
    /// [`super::stats::HarnessStats::by_rule`].
    pub counter: Option<Counter>,
}

/// THE ONE SORT (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.2.3, item
/// 1): what a ledger row under `rule_id` with `outcome`, typing `command`,
/// lights and counts — the stats table's own rows
/// ([`super::stats`]' module docs):
///
/// | Row | Act | Counter |
/// |---|---|---|
/// | `approved` under a box's rule | [`Act::Approved`] | [`Counter::Approved`] |
/// | `approved` under a question dialog's rule (`answer-*@v1`, `land-answer@v1`): one key of the dialog | [`Act::Answered`] | none: the journal's `chose` counts the dialog, once |
/// | `declined` | [`Act::Declined`] | [`Counter::Declined`] |
/// | `typed` `continue@v1`, `continue-suggestion@v1`, `continue-done-check@v1`, `continue-open-items@v1` | [`Act::Continued`] | [`Counter::Continued`] |
/// | `typed` `answer@v1`, `prose-*@v1`, `land-answer@v1` | [`Act::Answered`] | [`Counter::Answered`] |
/// | `typed` `land@v1` | [`Act::Landing`] | [`Counter::LandPrompts`] |
/// | `typed` `land-nudge@v1` | [`Act::Nudged`] | [`Counter::LandPrompts`] |
/// | `typed` `usage-resume@v1`, `api-retry@v1`, `api-back@v1`, `api-cutoff@v1`, and `context-compact@v1` typing anything but a slash command (the continuation) | [`Act::Resumed`] | [`Counter::Resumed`] |
/// | `typed` under any other rule (`context-compact@v1`'s `/compact` among them) | none | none |
/// | `skipped`, `deferred`, `waiting`, `escalated` | none | none |
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

/// Whether an `approved` row under `rule` is one key of a question dialog
/// (its tab, a toggle, its submit) rather than a box's press. Land's answer
/// is among them for the day Land answers dialogs: deferred (design §2.2),
/// so today a dialog under Land is keyed under the question rules, and
/// [`RULE_LAND_ANSWER`] is only ever typed in prose.
fn is_dialog_key(rule: &str) -> bool {
    is_question_rule(rule) || rule == RULE_LAND_ANSWER
}

/// Whether a `typed` row under `rule` answers a question asked in prose:
/// `answer_text` under any priority ([`QuestionPriority::prose_rule_id`]),
/// or Land's answer.
fn is_prose_answer(rule: &str) -> bool {
    rule == RULE_LAND_ANSWER
        || QuestionPriority::ALL
            .into_iter()
            .any(|p| p.prose_rule_id() == rule)
}

/// The act and counter of a `typed` row under `rule`, typing `command`;
/// `None`: one the page counts only by its rule.
fn typed_act(rule: &str, command: &str) -> Option<(Act, Counter)> {
    if matches!(
        rule,
        RULE_CONTINUE | RULE_SUGGESTION | RULE_DONE_CHECK | RULE_OPEN_ITEMS
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
        RULE_LIMIT_RESUME | RULE_API_RETRY | RULE_API_BACK | RULE_API_CUTOFF
    ) || (rule == RULE_COMPACT && !command.starts_with('/'))
    {
        Some((Act::Resumed, Counter::Resumed))
    } else {
        None
    }
}

/// The task's face under Land (§9.4.2), told by the loop as it moves.
///
/// | Face | Means | `task=` |
/// |---|---|---|
/// | [`TaskFace::Open`] | the work goes on, or a landing is under way | `open` |
/// | [`TaskFace::Landed`] | the agent finished after the land instruction | `landed` |
/// | [`TaskFace::Done`] | the agent said it was finished before a landing was asked | `done` |
/// | [`TaskFace::Spent`] | no done reply after the instruction and its nudges | `spent` |
/// | [`TaskFace::Theirs`] | a person's own exchange | `theirs` |
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaskFace {
    /// Nothing ended: the work goes on, or a landing is under way.
    Open,
    /// A done reply to a land act: the harness types nothing more until
    /// someone else's turn.
    Landed,
    /// The done check or `FINISHED` ended it, not Land: nothing more is
    /// typed, but nobody asked it to verify, commit and push.
    Done,
    /// The landing is spent: the instruction and its nudges typed, no done
    /// reply. Nothing more is typed but a land answer to a question.
    Spent,
    /// A person's own exchange: the harness types nothing until someone
    /// else's turn ends.
    Theirs,
}

impl TaskFace {
    /// Every face, in §9.4.1's order.
    pub const ALL: [TaskFace; 5] = [
        TaskFace::Open,
        TaskFace::Landed,
        TaskFace::Done,
        TaskFace::Spent,
        TaskFace::Theirs,
    ];

    /// The word `status` and `ls` print for it (`task=`).
    #[must_use]
    pub const fn status(self) -> &'static str {
        match self {
            TaskFace::Open => "open",
            TaskFace::Landed => "landed",
            TaskFace::Done => "done",
            TaskFace::Spent => "spent",
            TaskFace::Theirs => "theirs",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each act's glyph and word are its own pair (Land's two share the
    /// glyph and differ in the word), and its spoken words carry no glyph:
    /// the label's word, as §9.2.1 says it (`approved`), but Land's
    /// instruction (`started landing`), and no two acts say the same.
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

    /// The counter each act's family adds to, were an act its own counter:
    /// what [`sort`] gives everywhere but a dialog's key.
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

    /// THE SORT IS THE STATS TABLE'S: every rule each family counts lights
    /// that family's act and adds to its counter; a question dialog's key
    /// lights `Answered` and counts nothing; `context-compact@v1` typing
    /// `/compact` is nothing, and typing anything else is `Resumed` for
    /// both; the rules counted only by rule, and every `skipped`,
    /// `deferred`, `waiting` and `escalated` row, are nothing.
    ///
    /// NEGATIVE CONTROL: an act taken as its own counter (`Act::of`'s first
    /// draft, design §9.2.3) differs from the sort in exactly the dialog's
    /// keys — the one place the two must part — and nowhere else.
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
        // The usage-limit dialog's wait row is a box's press, as the page
        // counts it.
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
            RULE_SUGGESTION,
            RULE_DONE_CHECK,
            RULE_OPEN_ITEMS,
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
            // An older loop's save-then-wait row (the Codex harness, retired).
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
        // NEGATIVE CONTROL: the act as its own counter parts from the sort
        // at a dialog's keys alone.
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

    /// The five faces print five words.
    #[test]
    fn every_face_prints_its_own_status_word() {
        let words: std::collections::BTreeSet<_> =
            TaskFace::ALL.iter().map(|f| f.status()).collect();
        assert_eq!(
            words.into_iter().collect::<Vec<_>>(),
            ["done", "landed", "open", "spent", "theirs"]
        );
    }
}
