// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for question-dialog answer selection and focus movement.

use std::path::PathBuf;

use aterm_phase::prompt::fixtures::{self as f, screen};
use aterm_phase::{PromptKind, parse_prompt_v2};

use super::super::super::config::Approve;
use super::super::approval::{
    Answer, AnswerTarget, ApprovalCtx, Choice, Decision, QuestionPriority, decide,
};
use super::super::guard::row_guard;
use super::*;

fn recommended_answer(p: &aterm_phase::PromptV2, rows: &[String]) -> Decision {
    answer_question(
        p,
        rows,
        QuestionPriority::Recommended,
        None,
        false,
        Pane::of(rows),
    )
}

fn decided(text: &str) -> (Vec<String>, Decision) {
    let rows = screen(text);
    let p = parse_prompt_v2(&rows).unwrap_or_else(|| panic!("no box: {rows:#?}"));
    assert_eq!(p.kind, PromptKind::Question, "{rows:#?}");
    let d = recommended_answer(&p, &rows);
    (rows, d)
}

fn answer(d: &Decision) -> (i32, AnswerTarget, &str, &str, &str) {
    match d {
        Decision::Approve {
            rule_id,
            choice: Choice::Answer(Answer::FocusEnter { steps, target }),
            guard,
            subject,
            unproven,
        } => {
            assert_eq!(*rule_id, RULE_ANSWER_RECOMMENDED);
            (
                *steps,
                *target,
                guard.as_str(),
                subject.as_str(),
                unproven.as_deref().expect("an answer says how it chose"),
            )
        }
        other => panic!("not answered: {other:?}"),
    }
}

fn reason(d: &Decision) -> &str {
    match d {
        Decision::Escalate { reason } => reason,
        other => panic!("answered: {other:?}"),
    }
}

/// The row whose trimmed text is `text`.
pub(super) fn row_of(rows: &[String], text: &str) -> usize {
    rows.iter()
        .position(|r| r.trim() == text)
        .unwrap_or_else(|| panic!("no row {text:?}"))
}

/// `rows` with the `❯` focus moved from wherever it is onto row `to`.
pub(super) fn with_focus(rows: &[String], to: usize) -> Vec<String> {
    rows.iter()
        .enumerate()
        .map(|(k, r)| {
            let r = match r.strip_prefix('❯') {
                Some(rest) => format!(" {rest}"),
                None => r.clone(),
            };
            if k == to {
                let rest = r
                    .strip_prefix(' ')
                    .expect("a focusable row opens with a space");
                format!("❯{rest}")
            } else {
                r
            }
        })
        .collect()
}

fn decide_rows(rows: &[String]) -> Decision {
    let p = parse_prompt_v2(rows).unwrap_or_else(|| panic!("no box: {rows:#?}"));
    recommended_answer(&p, rows)
}

#[test]
fn the_incident_question_is_answered_with_its_recommended_option() {
    let (rows, d) = decided(f::QUESTION_TABS_INCIDENT);
    let (steps, target, guard, subject, _) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)));
    let q = rows
        .iter()
        .position(|r| r.starts_with("│ On a row with a progress bar"))
        .expect("the question row");
    assert_eq!(guard, row_guard(&rows[q]));
    assert!(subject.contains("Outlined on meters"), "{subject}");
}

#[test]
fn the_recommended_option_wins_over_the_focus() {
    let (_, d) = decided(f::QUESTION_RECOMMENDED_SECOND);
    let (steps, target, _, _, _) = answer(&d);
    assert_eq!((steps, target), (1, AnswerTarget::Option(2)));
}

#[test]
fn no_marked_option_answers_option_one() {
    let (_, d) = decided(f::QUESTION_NO_RECOMMENDATION);
    let (steps, target, _, _, _) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)));
}

#[test]
fn a_vendor_answer_is_kept_only_as_a_whole_label() {
    let rows: Vec<String> = screen(f::QUESTION_RECOMMENDED_SECOND)
        .iter()
        .map(|r| {
            r.replace(
                "Nextest (Recommended)",
                "Enable selected suites (Recommended)",
            )
            .replace(
                "Faster parallel runs with per-test isolation.",
                "Enable selected suites in parallel.",
            )
        })
        .collect();
    let d = decide_rows(&rows);
    let (steps, target, _, subject, _) = answer(&d);
    assert_eq!((steps, target), (1, AnswerTarget::Option(2)), "{d:?}");
    assert!(
        subject.ends_with("→ Enable selected suites (Recommended)"),
        "{subject}"
    );
    let rows: Vec<String> = screen(f::QUESTION_RECOMMENDED_SECOND)
        .iter()
        .map(|r| {
            r.replace("1. Built-in runner", "1. Enable selected")
                .replace("Nextest (Recommended)", "Nextest")
        })
        .collect();
    let d = decide_rows(&rows);
    assert!(reason(&d).contains("`Enable selected`"), "{d:?}");
    let rows: Vec<String> = screen(f::QUESTION_RECOMMENDED_SECOND)
        .iter()
        .map(|r| {
            r.replace(
                "Nextest (Recommended)",
                "Apply to all future runs (Recommended)",
            )
        })
        .collect();
    let d = decide_rows(&rows);
    let (steps, target, ..) = answer(&d);
    assert_eq!((steps, target), (1, AnswerTarget::Option(2)), "{d:?}");
}

#[test]
fn the_security_model_switch_stays_on_the_current_model() {
    let (_, d) = decided(f::QUESTION_MODEL_SWITCH_PREFERENCE);
    let (steps, target, _, subject, unproven) = answer(&d);
    assert_eq!((steps, target), (1, AnswerTarget::Option(2)));
    assert!(subject.ends_with("→ Stay on Opus 5.5"), "{subject}");
    assert_eq!(
        unproven,
        "a security alert's model switch: stayed on the current model (owner ruling, 2026-10-16)"
    );
}

#[test]
fn a_model_switch_without_the_alert_words_still_escalates() {
    let rows: Vec<String> = screen(f::QUESTION_MODEL_SWITCH_PREFERENCE)
        .iter()
        .map(|r| r.replace("safeguards flagged", "preferences changed"))
        .collect();
    let d = decide_rows(&rows);
    assert!(
        reason(&d).starts_with("a question whose answer `Switch automatically`"),
        "{d:?}"
    );
}

#[test]
fn a_lookalike_without_the_switch_companion_is_not_auto_stayed() {
    let rows: Vec<String> = screen(f::QUESTION_MODEL_SWITCH_PREFERENCE)
        .iter()
        .map(|r| {
            r.replace("1. Switch automatically", "1. Keep this one")
                .replace(
                    "Continue on Sonnet 5 now, and switch without asking from now on",
                    "Continue on Sonnet 5 for this message only",
                )
        })
        .collect();
    let d = decide_rows(&rows);
    let (steps, target, _, _, unproven) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)), "{d:?}");
    assert_eq!(unproven, "no option is marked (Recommended): option 1");
}

#[test]
fn a_destructive_lookalike_is_escalated_not_auto_stayed() {
    let rows: Vec<String> = screen(f::QUESTION_MODEL_SWITCH_PREFERENCE)
        .iter()
        .map(|r| r.replace("flagged this message", "flagged this delete"))
        .collect();
    let d = decide_rows(&rows);
    assert!(
        reason(&d).starts_with("a question with no recommended option that would delete"),
        "{d:?}"
    );
}

#[test]
fn a_wrapped_recommended_label_is_chosen() {
    let (_, d) = decided(f::QUESTION_LONG_LABEL_80COL);
    let (steps, target, ..) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)));
}

#[test]
fn focus_on_the_free_text_row_is_moved_up_never_answered() {
    let (_, d) = decided(f::QUESTION_FREE_TEXT_FOCUSED);
    let (steps, target, ..) = answer(&d);
    assert_eq!((steps, target), (-3, AnswerTarget::Option(1)));
}

#[test]
fn focus_on_the_chat_row_is_moved_up() {
    let (_, d) = decided(f::QUESTION_CHAT_FOCUSED);
    let (steps, target, ..) = answer(&d);
    assert_eq!((steps, target), (-3, AnswerTarget::Option(1)));
}

#[test]
fn a_persons_text_hands_the_question_over() {
    for text in [
        f::QUESTION_FREE_TEXT_TYPED,
        f::QUESTION_FREE_TEXT_TYPED_AWAY,
        f::QUESTION_MULTISELECT_TYPED,
    ] {
        let (_, d) = decided(text);
        assert!(reason(&d).contains("begun answering"), "{d:?}");
    }
}

#[test]
fn neither_type_something_nor_chat_is_ever_chosen() {
    use aterm_phase::{QuestionFocus, QuestionForm};
    let mut answered = 0;
    for text in [
        f::QUESTION_TABS_INCIDENT,
        f::QUESTION_TABS_SECOND,
        f::QUESTION_TABS_THIRD,
        f::QUESTION_TABS_FOURTH,
        f::QUESTION_REVIEW,
        f::QUESTION_REVIEW_UNANSWERED,
        f::QUESTION_ONE,
        f::QUESTION_RECOMMENDED_SECOND,
        f::QUESTION_FREE_TEXT_FOCUSED,
        f::QUESTION_MULTISELECT,
        f::QUESTION_MULTISELECT_CHECKED,
        f::QUESTION_MULTISELECT_NEXT,
        f::QUESTION_NO_RECOMMENDATION,
        f::QUESTION_PREVIEW,
        f::QUESTION_LONG_LABEL_80COL,
        f::QUESTION_LONG_LABEL_180COL,
        f::QUESTION_CHAT_FOCUSED,
        f::QUESTION_AFK,
        f::QUESTION_DO_YOU_WANT_LIVE,
        f::QUESTION_MULTISELECT_ALONE,
    ] {
        let rows = screen(text);
        let dialog = parse_prompt_v2(&rows)
            .and_then(|p| p.question_dialog)
            .expect("a dialog");
        let focusable: Vec<QuestionFocus> = match &dialog.form {
            QuestionForm::Single { options, .. } => options
                .iter()
                .map(|o| QuestionFocus::Option(o.n))
                .chain([QuestionFocus::FreeText, QuestionFocus::Chat])
                .collect(),
            QuestionForm::Multi { options, .. } => options
                .iter()
                .map(|o| QuestionFocus::Option(o.n))
                .chain([
                    QuestionFocus::FreeText,
                    QuestionFocus::Button,
                    QuestionFocus::Chat,
                ])
                .collect(),
            QuestionForm::Preview { options, .. } => options
                .iter()
                .map(|o| QuestionFocus::Option(o.n))
                .chain([QuestionFocus::Chat])
                .collect(),
            QuestionForm::Review { .. } => {
                vec![QuestionFocus::ReviewSubmit, QuestionFocus::ReviewCancel]
            }
            QuestionForm::Unknown { why } => panic!("not read whole: {why}"),
        };
        for at in focusable {
            let row = focus_row(&dialog, at).expect("a focusable row");
            let moved = with_focus(&rows, row);
            let d = decide_rows(&moved);
            let Decision::Approve {
                choice: Choice::Answer(Answer::FocusEnter { steps, target }),
                ..
            } = d
            else {
                continue;
            };
            answered += 1;
            let options = match &dialog.form {
                QuestionForm::Single { options, .. }
                | QuestionForm::Multi { options, .. }
                | QuestionForm::Preview { options, .. } => options.len(),
                _ => 0,
            };
            match target {
                AnswerTarget::Option(n) => assert!(
                    (1..=options).contains(&usize::from(n)),
                    "{text:.60}: option {n} of {options}"
                ),
                AnswerTarget::Button => {
                    assert!(matches!(dialog.form, QuestionForm::Multi { .. }))
                }
                AnswerTarget::ReviewSubmit => {
                    assert!(matches!(dialog.form, QuestionForm::Review { .. }))
                }
                AnswerTarget::OwnText(p) => panic!("{text:.60}: a policy text under {p:?}"),
            }
            let from = focus_position(&dialog, at).expect("in the order");
            let to = focus_position(&dialog, target_focus(target)).expect("in the order");
            assert_eq!(steps, to - from, "{text:.60} from {at:?}");
        }
    }
    assert!(answered > 60, "the sweep answered {answered} variants");
}

#[test]
fn multiselect_toggles_each_recommended_then_advances() {
    let (_, d) = decided(f::QUESTION_MULTISELECT);
    let (steps, target, ..) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)));
    let (_, d) = decided(f::QUESTION_MULTISELECT_CHECKED);
    let (steps, target, ..) = answer(&d);
    assert_eq!((steps, target), (2, AnswerTarget::Button));
    let (_, d) = decided(f::QUESTION_MULTISELECT_NEXT);
    assert_eq!(
        (answer(&d).0, answer(&d).1),
        (0, AnswerTarget::Button),
        "{d:?}"
    );
}

#[test]
fn multiselect_with_a_foreign_check_hands_over() {
    let rows = screen(f::QUESTION_MULTISELECT);
    let edited: Vec<String> = rows
        .iter()
        .map(|r| r.replace("2. [ ] Emoji", "2. [✔] Emoji"))
        .collect();
    assert_ne!(rows, edited, "PRECONDITION: the edit landed");
    let d = decide_rows(&edited);
    assert!(reason(&d).contains("option 2 is checked"), "{d:?}");
    assert!(matches!(decide_rows(&rows), Decision::Approve { .. }));
}

#[test]
fn the_preview_form_is_focused_then_entered() {
    let (rows, d) = decided(f::QUESTION_PREVIEW);
    assert_eq!((answer(&d).0, answer(&d).1), (0, AnswerTarget::Option(1)));
    let three = rows
        .iter()
        .position(|r| r.starts_with("  3. Single scroll"))
        .expect("option 3");
    let d = decide_rows(&with_focus(&rows, three));
    assert_eq!((answer(&d).0, answer(&d).1), (-2, AnswerTarget::Option(1)));
    let (_, d) = decided(f::QUESTION_PREVIEW_REVISITED);
    assert!(reason(&d).contains("ticked ✔"), "{d:?}");
}

#[test]
fn the_review_tab_submits() {
    let (rows, d) = decided(f::QUESTION_REVIEW);
    let (steps, target, guard, _, _) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::ReviewSubmit));
    assert_eq!(
        guard,
        row_guard(&rows[row_of(&rows, "❯ 1. Submit answers")])
    );
    let (rows, d) = decided(f::QUESTION_REVIEW_UNANSWERED);
    let (steps, target, _, subject, _) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::ReviewSubmit));
    assert!(subject.contains("Placement"), "{subject}");
    let cancel = with_focus(&rows, row_of(&rows, "2. Cancel"));
    let d = decide_rows(&cancel);
    assert_eq!(
        (answer(&d).0, answer(&d).1),
        (-1, AnswerTarget::ReviewSubmit)
    );
}

#[test]
fn an_unmarked_destructive_question_is_answered_by_its_refusal_or_is_the_persons() {
    let rows = screen(f::QUESTION_DO_YOU_WANT_LIVE);
    assert!(matches!(decide_rows(&rows), Decision::Approve { .. }));
    let drop: Vec<String> = rows
        .iter()
        .map(|r| r.replace("Run the migration now.", "Drop the staging table now."))
        .collect();
    assert_ne!(rows, drop, "PRECONDITION: the edit landed");
    let refused = decide_rows(&drop);
    let (steps, target, ..) = answer(&refused);
    assert_eq!((steps, target), (1, AnswerTarget::Option(2)), "the one No");
    let marked: Vec<String> = drop
        .iter()
        .map(|r| r.replace("1. Yes", "1. Yes (Recommended)"))
        .collect();
    let (steps, target, ..) = answer(&decide_rows(&marked));
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)));
    for pairs in [
        &[("Leave the schema as it is.", "Delete the backup instead.")][..],
        &[
            ("Do you want to proceed?", "Drop the staging table?"),
            ("1. Yes", "1. No, not yet"),
            ("Drop the staging table now.", "Wait for the backup."),
        ][..],
    ] {
        let edited: Vec<String> = drop
            .iter()
            .map(|r| {
                pairs
                    .iter()
                    .fold(r.clone(), |r, (from, to)| r.replace(from, to))
            })
            .collect();
        let to = pairs[0].1;
        assert_ne!(edited, drop, "PRECONDITION: the edit landed");
        assert!(
            reason(&decide_rows(&edited)).contains("no one option that refuses it"),
            "{to}"
        );
    }
    let rows = screen(f::QUESTION_NO_RECOMMENDATION);
    assert!(matches!(decide_rows(&rows), Decision::Approve { .. }));
    let drops: Vec<String> = rows
        .iter()
        .map(|r| r.replace("Above the tab strip.", "Drops the legacy table."))
        .collect();
    assert_ne!(rows, drops, "PRECONDITION: the edit landed");
    let passed = decide_rows(&drops);
    let (steps, target, _, subject, unproven) = answer(&passed);
    assert_eq!(
        (steps, target),
        (1, AnswerTarget::Option(2)),
        "option 1's own words drop a table, the question asks nothing irreversible: the first \
         option that names nothing irreversible"
    );
    assert!(subject.ends_with("→ Bottom"), "{subject}");
    assert!(unproven.contains("option 1 would drop"), "{unproven}");
    let every: Vec<String> = drops
        .iter()
        .map(|r| {
            r.replace("Below the last terminal row.", "Removes the old band.")
                .replace("An overlay in the corner.", "Deletes the overlay layer.")
        })
        .collect();
    assert_ne!(every, drops, "PRECONDITION: the edit landed");
    let held = decide_rows(&every);
    assert!(
        reason(&held).contains("every option names a destructive act")
            && reason(&held).contains("option 1 would drop"),
        "{held:?}"
    );
}

#[test]
fn the_incidents_unmarked_publish_question_is_escalated() {
    let incident = |refusal: &str| -> Vec<String> {
        screen(f::QUESTION_NO_RECOMMENDATION)
            .iter()
            .map(|r| {
                [
                    (
                        "Where should the message band sit?",
                        "Want me to publish this now?",
                    ),
                    ("1. Top", "1. Deploy and push"),
                    ("2. Bottom", "2. Push only"),
                    ("3. Floating", refusal),
                ]
                .iter()
                .fold(r.clone(), |r, (from, to)| r.replace(from, to))
            })
            .collect()
    };
    let rows = incident("3. Hold everything");
    assert!(
        rows.iter().any(|r| r.trim() == "❯ 1. Deploy and push"),
        "PRECONDITION: the edit landed, the cursor on option 1: {rows:#?}"
    );
    assert!(
        reason(&decide_rows(&rows)).contains("would publish, and no one option that refuses it"),
        "the question's own row asks to publish: every answer to it bears on that"
    );
    let neutral: Vec<String> = rows
        .iter()
        .map(|r| {
            r.replace("Want me to publish this now?", "Which should come first?")
                .replace("1. Deploy and push", "1. Tests and docs")
        })
        .collect();
    assert_eq!(answer(&decide_rows(&neutral)).1, AnswerTarget::Option(1));
    let refused = incident("3. No, hold everything");
    assert_eq!(answer(&decide_rows(&refused)).1, AnswerTarget::Option(3));
}

/// The 2026-10-24 hold (`claude-2.1.310-question-reset-in-description.txt`): an AskUserQuestion
/// under Land with no option marked (Recommended) whose question asks nothing irreversible, but
/// whose option 1 describes a bug in words holding `reset`, was escalated as "would reset, and no
/// one option that refuses it" and waited 44 minutes for the person. Option 1 is passed over (its
/// own words name the act) and the harness answers with the first option that names nothing
/// irreversible.
#[test]
fn a_destructive_word_in_one_options_description_never_holds_the_question() {
    let (rows, d) = decided(f::QUESTION_RESET_IN_DESCRIPTION_2_1_310);
    let (steps, target, guard, subject, unproven) = answer(&d);
    assert_eq!((steps, target), (1, AnswerTarget::Option(2)), "{d:?}");
    assert!(subject.ends_with("→ Split = codex-alab first"), "{subject}");
    assert!(
        unproven.starts_with(CHOSE_FIRST_SAFE) && unproven.contains("option 1 would reset"),
        "{unproven}"
    );
    let q = row_of(
        &rows,
        "Which did you mean by \"no no claude code harness, codex-alab\"?",
    );
    assert_eq!(guard, row_guard(&rows[q]));
    let clean: Vec<String> = rows
        .iter()
        .map(|r| r.replace("usage limit reset", "usage limit ended"))
        .collect();
    assert_ne!(clean, rows, "PRECONDITION: the edit landed");
    let fallback = decide_rows(&clean);
    let (steps, target, _, _, unproven) = answer(&fallback);
    assert_eq!(
        (steps, target, unproven),
        (0, AnswerTarget::Option(1), CHOSE_UNMARKED),
        "with no destructive word option 1 is the documented fallback"
    );
}

/// The same incident's question under every priority word: a text priority hands the question
/// back to the recommended path (its options name a destructive word), which answers it too.
#[test]
fn the_reset_in_a_description_incident_is_answered_under_every_priority() {
    let rows = screen(f::QUESTION_RESET_IN_DESCRIPTION_2_1_310);
    let p = parse_prompt_v2(&rows).expect("the dialog");
    for priority in QuestionPriority::ALL {
        let d = answer_question(&p, &rows, priority, None, false, Pane::of(&rows));
        let Decision::Approve {
            choice: Choice::Answer(Answer::FocusEnter { steps, target }),
            ..
        } = d
        else {
            panic!("{priority:?} held the question: {d:?}");
        };
        assert_eq!(
            (steps, target),
            (1, AnswerTarget::Option(2)),
            "{priority:?}"
        );
    }
}

#[test]
fn a_refusal_is_no_and_never_a_word_that_starts_with_it() {
    for label in [
        "No",
        "no",
        "No, keep it",
        "No — leave it",
        "No (skip)",
        "No.",
    ] {
        assert!(super::refuses(label), "{label}");
    }
    for label in ["None of the above", "Not now", "Now", "Node", "Yes", ""] {
        assert!(!super::refuses(label), "{label}");
    }
}

#[test]
fn muses_question_is_answered_by_the_cursor_and_enter() {
    use aterm_phase::muse::fixtures as u;
    let muse = |rows: &[String]| {
        let reading = aterm_phase::read(Some("muse"), rows, None);
        let p = reading.prompt.expect("the question");
        let d = recommended_answer(&p, rows);
        (p, d)
    };
    let focus = |d: &Decision| match d {
        Decision::Approve {
            choice: Choice::Focus { steps, label },
            guard,
            ..
        } => (*steps, label.clone(), guard.clone()),
        other => panic!("not focused: {other:?}"),
    };
    let question_row = "  Which color should the probe use?";
    for (text, steps) in [
        (u::QUESTION, 0),
        (u::QUESTION_DOWN, -1),
        (u::QUESTION_NONE, -2),
    ] {
        let rows = screen(text);
        let (_, d) = muse(&rows);
        let (got, label, guard) = focus(&d);
        assert_eq!(
            (got, label.as_str()),
            (steps, "Red (Recommended)"),
            "{text}"
        );
        assert_eq!(guard, row_guard(question_row));
        assert!(
            !rows.iter().any(|r| r.contains("Request user input")
                && aterm_observe::row_matcher(&guard).is_ok_and(|m| m.matches(r))),
            "never the head"
        );
    }
    let rows = screen(u::QUESTION);
    let unmarked: Vec<String> = rows
        .iter()
        .map(|r| r.replace(" (Recommended)", ""))
        .collect();
    let (_, d) = muse(&unmarked);
    assert_eq!(focus(&d).1, "Red");
    let destructive: Vec<String> = unmarked
        .iter()
        .map(|r| {
            r.replace(
                "Which color should the probe use?",
                "Should I delete the probe file?",
            )
            .replace("1. Red ", "1. Yes ")
            .replace("2. Blue", "2. No  ")
        })
        .collect();
    let (_, d) = muse(&destructive);
    assert_eq!(focus(&d).0, 1, "its one No: {d:?}");
    let (p, _) = muse(&rows);
    let long = answer_question(
        &p,
        &rows,
        QuestionPriority::LongTerm,
        None,
        false,
        Pane::of(&rows),
    );
    assert!(
        matches!(
            &long,
            Decision::Approve {
                choice: Choice::Note { text: QuestionPriority::LongTerm },
                guard,
                ..
            } if *guard == row_guard(question_row)
        ),
        "under long-term its note, guarded on the question's row: {long:?}"
    );
    let lost: Vec<String> = rows.iter().map(|r| r.replace("⟩ 1.", "  1.")).collect();
    let (p, d) = muse(&lost);
    assert!(p.options.iter().all(|o| !o.focused));
    assert!(
        reason(&d).starts_with("a question aterm could not read whole"),
        "{d:?}"
    );
}

#[test]
fn an_unread_question_is_escalated() {
    for text in [
        f::QUESTION_YES_NO,
        f::QUESTION_DO_YOU_WANT,
        f::QUESTION_SCROLLED,
        f::QUESTION_DEFERRED,
    ] {
        let rows = screen(text);
        let p = parse_prompt_v2(&rows).expect("a box");
        let d = recommended_answer(&p, &rows);
        assert!(
            reason(&d).starts_with("a question aterm could not read whole"),
            "{d:?}"
        );
    }
    let rows = screen(f::QUESTION_DEFERRED);
    let p = parse_prompt_v2(&rows).expect("a box");
    assert!(
        reason(&recommended_answer(&p, &rows)).contains("draft"),
        "the badge names the draft"
    );
}

fn ctx(approve: Approve, answer_questions: bool) -> ApprovalCtx {
    let mut c = ApprovalCtx::new(
        PathBuf::from("/tmp/work"),
        None,
        501,
        None,
        std::sync::Arc::new(crate::supervise::policy::LiveProbe),
    );
    c.approve = approve;
    c.answer_questions = answer_questions;
    c
}

fn decide_with(text: &str, approve: Approve, answer_questions: bool) -> Decision {
    let rows = screen(text);
    let reading = aterm_phase::read(Some("claude"), &rows, None);
    decide(&reading, &rows, &ctx(approve, answer_questions))
}

#[test]
fn answer_questions_off_hands_every_question_over() {
    for approve in [Approve::All, Approve::Safe, Approve::None] {
        let d = decide_with(f::QUESTION_TABS_INCIDENT, approve, false);
        assert!(reason(&d).contains("answer_questions"), "{approve:?}");
    }
}

#[test]
fn answer_questions_is_independent_of_approve() {
    for approve in [Approve::All, Approve::Safe, Approve::None] {
        let d = decide_with(f::QUESTION_TABS_INCIDENT, approve, true);
        assert!(
            matches!(d, Decision::Approve { rule_id, .. } if rule_id == RULE_ANSWER_RECOMMENDED),
            "{approve:?}: {d:?}"
        );
    }
    let d = decide_with(f::QUESTION_TABS_INCIDENT, Approve::All, false);
    assert!(matches!(d, Decision::Escalate { .. }), "{d:?}");
}

#[test]
fn a_cut_question_is_escalated() {
    let rows = screen(f::QUESTION_TABS_INCIDENT);
    let opt2 = rows
        .iter()
        .position(|r| r.starts_with("  2. Solid everywhere"))
        .expect("option 2");
    let tail = rows[opt2..].to_vec();
    let reading = aterm_phase::read(Some("claude"), &tail, None);
    let d = decide(&reading, &tail, &ctx(Approve::All, true));
    assert!(
        reason(&d).starts_with("a question aterm could not read whole"),
        "{d:?}"
    );
}

#[test]
fn no_visible_focus_is_never_answered() {
    let rows = screen(f::QUESTION_TABS_INCIDENT);
    let bare: Vec<String> = rows
        .iter()
        .map(|r| match r.strip_prefix('❯') {
            Some(rest) if r.contains("1. Outlined") => format!(" {rest}"),
            _ => r.clone(),
        })
        .collect();
    assert_ne!(rows, bare, "PRECONDITION: the focus was removed");
    assert!(matches!(decide_rows(&bare), Decision::Escalate { .. }));
}

type ModelState = std::collections::BTreeMap<&'static str, i64>;

/// What a decision on a question comes to, in the model's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    /// Enter where the focus is (`HarnessEnter`).
    Enter,
    /// A move toward the chosen row first (`HarnessMove`: `↑` or `↓`).
    Up,
    /// Nothing keyed: the person's.
    Escalate,
}

fn verdict(d: &Decision) -> Verdict {
    match d {
        Decision::Approve {
            choice:
                Choice::Answer(Answer::FocusEnter {
                    steps,
                    target: AnswerTarget::Option(1) | AnswerTarget::ReviewSubmit,
                }),
            ..
        } if *steps <= 0 => {
            if *steps == 0 {
                Verdict::Enter
            } else {
                Verdict::Up
            }
        }
        Decision::Escalate { .. } => Verdict::Escalate,
        other => panic!("a decision the model has no word for: {other:?}"),
    }
}

fn decision_of(model: &aterm_spec::derive::Model, st: &ModelState) -> Verdict {
    let mut free = st.clone();
    for (var, v) in [
        ("quiet", 1),
        ("seen_quiet", 1),
        ("touched", 0),
        ("mine", 0),
        ("stilled", 0),
        ("retried", 0),
        ("q1", 0),
        ("q2", 0),
        ("fresh", 1),
    ] {
        free.insert(var, v);
    }
    match (
        model.action_enabled("HarnessEnter", &free),
        model.action_enabled("HarnessMove", &free),
    ) {
        (true, false) => Verdict::Enter,
        (false, true) => Verdict::Up,
        (false, false) => Verdict::Escalate,
        (true, true) => panic!("the model enables both keys at {st:?}"),
    }
}

fn drawn(tab: i64, tabs: i64, focus: i64, row: i64, typed: i64, on_chat: bool) -> Vec<String> {
    if tab == tabs {
        let rows = screen(f::QUESTION_REVIEW);
        return if focus == 0 {
            rows
        } else {
            let cancel = row_of(&rows, "2. Cancel");
            with_focus(&rows, cancel)
        };
    }
    let mut rows = screen(if tab == 0 {
        f::QUESTION_TABS_THIRD
    } else {
        f::QUESTION_TABS_FOURTH
    });
    let free = row_of(&rows, "3. Type something.");
    if typed == 1 {
        rows[free] = rows[free].replace("Type something.", "2");
    }
    if focus == 0 {
        return rows;
    }
    let to = match (row, on_chat) {
        (0, _) => free,
        (_, false) => rows
            .iter()
            .position(|r| r.trim_start().starts_with("2. "))
            .expect("option 2"),
        (_, true) => row_of(&rows, "4. Chat about this"),
    };
    with_focus(&rows, to)
}

#[test]
fn tier1_the_question_decider_chooses_only_what_the_model_enables() {
    let model = aterm_models_harness::supervisor_question_answer_model();
    let tabs = model
        .consts
        .iter()
        .find(|(n, _)| *n == "Tabs")
        .map(|(_, v)| *v)
        .expect("Tabs");
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([model.init_state()]);
    let mut judged = std::collections::BTreeSet::new();
    let mut verdicts = std::collections::BTreeSet::new();
    while let Some(st) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        for a in &model.actions {
            let mut next = st.clone();
            if model.fire(a.name, &mut next) {
                queue.push_back(next);
            }
        }
        if st["fresh"] != 1 || st["seen_tab"] > tabs {
            continue;
        }
        let read = (
            st["seen_tab"],
            st["seen_focus"],
            st["seen_row"],
            st["seen_typed"],
        );
        if !judged.insert(read) {
            continue;
        }
        let want = decision_of(&model, &st);
        for on_chat in [false, true] {
            let rows = drawn(read.0, tabs, read.1, read.2, read.3, on_chat);
            let got = verdict(&decide_rows(&rows));
            assert_eq!(
                got, want,
                "the real decider disagrees with the model at the read {read:?} \
                 (on_chat {on_chat}): {rows:#?}"
            );
            verdicts.insert((read.0 == tabs, got));
        }
    }
    for v in [Verdict::Enter, Verdict::Up, Verdict::Escalate] {
        assert!(verdicts.contains(&(false, v)), "no {v:?} on a question tab");
    }
    for v in [Verdict::Enter, Verdict::Up] {
        assert!(verdicts.contains(&(true, v)), "no {v:?} on the review");
    }
    let typed_away = model
        .init_state()
        .into_iter()
        .map(|(k, v)| match k {
            "fresh" | "seen_typed" | "typed" => (k, 1),
            _ => (k, v),
        })
        .collect::<ModelState>();
    let want = decision_of(&model, &typed_away);
    assert_eq!(want, Verdict::Escalate);
    assert_eq!(verdict(&decide_rows(&drawn(0, tabs, 0, 0, 1, false))), want);
    let blind = verdict(&decide_rows(&drawn(0, tabs, 0, 0, 0, false)));
    assert_eq!(blind, Verdict::Enter);
    assert_ne!(
        blind, want,
        "the comparison would not catch the blind decider"
    );
}
