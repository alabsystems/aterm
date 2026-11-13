// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The driver line the window writes under either head is the one the lab reads; a head
/// alone, a worker's line or a longer word is none.
#[test]
fn a_driver_line_under_either_head_reads_as_one() {
    for head in [ATERM_DRIVES, CODEX_ALAB_DRIVES] {
        assert!(is_driver_line(&driver_line(head, "land: not landed yet")));
        assert!(!is_driver_line(head));
    }
    assert!(!is_driver_line("worker of manager"));
    assert!(!is_driver_line("driven by atermx (land)"));
}

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
                stop_words(held, form, false).starts_with(STOPPED_TYPING),
                "{held:?} {form:?}"
            );
        }
        let spoken = stop_words(held, StopForm::Spoken, true);
        assert!(spoken.ends_with('.'), "{spoken}");
        for form in [StopForm::Title, StopForm::Line] {
            assert!(
                stop_words(held, form, true).ends_with(TYPES_AGAIN_OR_SWITCH),
                "{held:?} {form:?}: the switch named where it asks"
            );
            assert!(
                stop_words(held, form, false).ends_with(TYPES_AGAIN),
                "{held:?} {form:?}: no switch named where it would not ask"
            );
        }
        for form in [StopForm::Hover, StopForm::Tip] {
            assert_eq!(stop_words(held, form, true), stop_words(held, form, false));
        }
    }
    assert_eq!(HeldBy::of_word("-"), None);
    assert_ne!(
        stop_words(HeldBy::SameReply, StopForm::Line, false),
        stop_words(HeldBy::NoWork, StopForm::Line, false),
        "the arms say different whys"
    );
}

/// `status land_held=` and `meta set harness_land legs=` read back to the leg each word
/// names, and no other word, nor a word in another case, names one; each leg has a bit of its
/// own, the left leg 64.
#[test]
fn a_holds_leg_round_trips_its_word() {
    for leg in HoldLeg::ALL {
        assert_eq!(HoldLeg::of_word(leg.word()), Some(leg));
    }
    for word in [
        "",
        "-",
        "unread",
        "REPO",
        "repo,open",
        "needs ",
        "LEFT",
        "leftover",
    ] {
        assert_eq!(HoldLeg::of_word(word), None, "{word:?}");
    }
    assert_eq!((HoldLeg::Left.bit(), HoldLeg::Left.word()), (64, "left"));
    let every = HoldLeg::ALL.iter().fold(0u16, |bits, leg| {
        assert_eq!(bits & u16::from(leg.bit()), 0, "{leg:?} shares a bit");
        bits | u16::from(leg.bit())
    });
    assert_eq!(every, 127);
    assert_eq!(left_words(1), "1 process left running");
    assert_eq!(left_words(3), "3 processes left running");
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
    RULE_USAGE_PROBE,
};

fn family(act: Act) -> Counter {
    match act {
        Act::Approved => Counter::Approved,
        Act::Declined => Counter::Declined,
        Act::Answered => Counter::Answered,
        Act::Continued => Counter::Continued,
        Act::Resumed => Counter::Resumed,
        Act::Landing | Act::Nudged | Act::LandCheck => Counter::LandPrompts,
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
        RULE_GO_AHEAD,
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
    assert_eq!(
        sort(RULE_LAND_CHECK, typed, "land check"),
        lit(Act::LandCheck, Counter::LandPrompts)
    );
    for rule in [
        RULE_LIMIT_RESUME,
        RULE_API_RETRY,
        RULE_API_BACK,
        RULE_API_CUTOFF,
        RULE_OUTAGE_RESUME,
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
        "model-fallback@v1",
        "model-restore@v1",
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
    let words: std::collections::BTreeSet<_> = TaskFace::ALL.iter().map(|f| f.status()).collect();
    assert_eq!(
        words.into_iter().collect::<Vec<_>>(),
        ["failed", "landed", "open", "theirs"]
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
        ("plan-approve@v2", Some("approve")),
        ("answer-recommended@v1", Some("answer")),
        ("answer-long-term@v1", Some("answer")),
        ("answer-short-term@v1", Some("answer")),
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
        ("api-back@v1", Some("continue")),
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
        RULE_GO_AHEAD,
        RULE_ANSWER,
        RULE_PROSE_LONG_TERM,
        RULE_PROSE_SHORT_TERM,
        RULE_API_RETRY,
        RULE_API_BACK,
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
    for rule in [
        RULE_APPROVE_ALL,
        RULE_PLAN,
        RULE_ANSWER_RECOMMENDED,
        RULE_ANSWER_LONG_TERM,
        RULE_ANSWER_SHORT_TERM,
    ] {
        let loops = log_mark(rule, Outcome::Approved, "").expect("a box's act");
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
        ("outage-resume@v1", true),
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
    assert!(!worded("outage-resume@v1") && !worded("mail-report@v1"));
}

/// A SELF-CLAIM'S ACT LIGHTS AND COUNTS AS ATERM'S OWN ACT OF ITS RULE: a continuation keeps
/// going, a wall's carry resumes, a land act is a land prompt, an answer answers, and its
/// box rules (approve-all, the plan box, a question box under every priority) light what
/// aterm's own approval of the box lights, each counted once. NEGATIVE CONTROL: its landing,
/// its re-exec, a point left to a person, a mail turn and a word no rule is light nothing and
/// count nothing.
#[test]
fn a_self_claims_act_sorts_as_the_typed_act_of_its_rule() {
    let lit = |act, counter| Sorted {
        act: Some(act),
        counter: Some(counter),
    };
    let cases = [
        ("approve-all@v1", lit(Act::Approved, Counter::Approved)),
        ("plan-approve@v2", lit(Act::Approved, Counter::Approved)),
        (
            "answer-recommended@v1",
            lit(Act::Answered, Counter::Answered),
        ),
        ("answer-long-term@v1", lit(Act::Answered, Counter::Answered)),
        (
            "answer-short-term@v1",
            lit(Act::Answered, Counter::Answered),
        ),
        ("answer@v1", lit(Act::Answered, Counter::Answered)),
        ("land-answer@v1", lit(Act::Answered, Counter::Answered)),
        ("prose-long-term@v1", lit(Act::Answered, Counter::Answered)),
        ("continue@v1", lit(Act::Continued, Counter::Continued)),
        (
            "policy-continue@v1",
            lit(Act::Continued, Counter::Continued),
        ),
        (
            "continue-done-check@v1",
            lit(Act::Continued, Counter::Continued),
        ),
        ("usage-resume@v1", lit(Act::Resumed, Counter::Resumed)),
        ("api-retry@v1", lit(Act::Resumed, Counter::Resumed)),
        ("api-back@v1", lit(Act::Resumed, Counter::Resumed)),
        ("context-compact@v1", lit(Act::Resumed, Counter::Resumed)),
        ("land@v1", lit(Act::Landing, Counter::LandPrompts)),
        ("land-nudge@v1", lit(Act::Nudged, Counter::LandPrompts)),
        ("landed@v1", Sorted::default()),
        ("upgrade@v1", Sorted::default()),
        ("left-to-person@v1", Sorted::default()),
        ("harness-turn@v1", Sorted::default()),
        ("mail-report@v1", Sorted::default()),
        ("approve-all@v2", Sorted::default()),
        ("continue", Sorted::default()),
    ];
    let wrong: Vec<_> = cases
        .iter()
        .filter(|(act, want)| self_claim_sort(act) != *want)
        .map(|(act, want)| (act, want, self_claim_sort(act)))
        .collect();
    assert!(wrong.is_empty(), "{wrong:?}");
    for (act, want) in cases {
        if let Some((typed, counter)) = typed_act(act, "") {
            assert_eq!(
                want,
                lit(typed, counter),
                "{act}: aterm's own act of the rule"
            );
        }
    }
    for act in [
        RULE_APPROVE_ALL,
        RULE_PLAN,
        RULE_ANSWER_RECOMMENDED,
        RULE_ANSWER_LONG_TERM,
        RULE_ANSWER_SHORT_TERM,
    ] {
        let own = sort(act, Outcome::Approved, "");
        assert!(own.act.is_some(), "{act}: aterm's own approval lights it");
        assert_eq!(
            self_claim_sort(act).act,
            own.act,
            "{act}: as aterm's own box"
        );
        assert!(
            self_claim_sort(act).counter.is_some(),
            "{act}: counted once"
        );
    }
}

/// LAND'S DIALOG ANSWER (`answer-land@v1`) IS AN ANSWER: its approved key lights Answered and
/// counts nothing, as every dialog key does; a self-claim of it is an answer counted once,
/// which starts no turn; its log mark is `act word=answer`. NEGATIVE CONTROL: the recommended
/// answer's rows sort as before. MUTANT: Land left out of `QuestionPriority::EVERY` in
/// `is_question_rule`.
#[test]
fn lands_dialog_answer_sorts_as_an_answer() {
    let land = "answer-land@v1";
    assert_eq!(land, RULE_ANSWER_LAND);
    for rule in [land, RULE_ANSWER_RECOMMENDED] {
        assert_eq!(
            sort(rule, Outcome::Approved, ""),
            Sorted {
                act: Some(Act::Answered),
                counter: None,
            },
            "{rule}"
        );
        assert_eq!(
            self_claim_sort(rule),
            Sorted {
                act: Some(Act::Answered),
                counter: Some(Counter::Answered),
            },
            "{rule}"
        );
        assert_eq!(
            self_claim_act(rule).map(|(w, _)| w),
            Some("answer"),
            "{rule}"
        );
        assert!(!self_claim_types_turn(rule), "{rule}");
    }
    assert_eq!(
        log_mark(land, Outcome::Approved, "").as_deref(),
        Some("act word=answer rule=answer-land.v1")
    );
}

/// A HOLDER'S NAME DROPS ONLY THE PID ITS PROOF NAMES. NEGATIVE CONTROL: a suffix that is
/// no pid, or a holder with no name before it, is the holder whole.
#[test]
fn a_claimants_name_is_its_holder_without_the_pid() {
    assert_eq!(claimant_name("codex-alab@4242"), "codex-alab");
    assert_eq!(claimant_name("x@y@17"), "x@y");
    for whole in [
        "codex-alab",
        "codex-alab@",
        "codex-alab@v1",
        "@4242",
        "a@12b",
    ] {
        assert_eq!(claimant_name(whole), whole, "{whole}");
    }
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
