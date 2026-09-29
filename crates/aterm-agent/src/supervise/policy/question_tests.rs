// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The question answer over aterm-phase's fixtures of Claude Code 2.1.282's
//! question dialog (the live captures of 2026-09-25 and the hand-built shapes
//! the critique of that day named): which row is chosen, how the focus gets
//! there, and every shape that is the person's.

use std::path::PathBuf;

use aterm_phase::prompt::fixtures::{self as f, screen};
use aterm_phase::{PromptKind, parse_prompt_v2};

use super::super::super::config::Approve;
use super::super::approval::{Answer, AnswerTarget, ApprovalCtx, Choice, Decision, decide};
use super::super::guard::row_guard;
use super::*;

/// A fixture's rows and the decision on its question.
fn decided(text: &str) -> (Vec<String>, Decision) {
    let rows = screen(text);
    let p = parse_prompt_v2(&rows).unwrap_or_else(|| panic!("no box: {rows:#?}"));
    assert_eq!(p.kind, PromptKind::Question, "{rows:#?}");
    let d = answer_question(&p, &rows);
    (rows, d)
}

/// The answer a decision chose: its steps and target, its guard, subject and
/// unproven.
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
fn row_of(rows: &[String], text: &str) -> usize {
    rows.iter()
        .position(|r| r.trim() == text)
        .unwrap_or_else(|| panic!("no row {text:?}"))
}

/// `rows` with the `❯` focus moved from wherever it is onto row `to`.
fn with_focus(rows: &[String], to: usize) -> Vec<String> {
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

/// The decision on edited rows.
fn decide_rows(rows: &[String]) -> Decision {
    let p = parse_prompt_v2(rows).unwrap_or_else(|| panic!("no box: {rows:#?}"));
    answer_question(&p, rows)
}

/// F2: the incident's screen (S6-01) is answered with its recommended
/// option by one Enter where the vendor put the focus, guarded on the
/// question's first row as drawn; the ledger names the question and the
/// label.
#[test]
fn the_incident_question_is_answered_with_its_recommended_option() {
    let (rows, d) = decided(f::QUESTION_TABS_INCIDENT);
    let (steps, target, guard, subject, unproven) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)));
    let q = rows
        .iter()
        .position(|r| r.starts_with("│ On a row with a progress bar"))
        .expect("the question row");
    assert_eq!(guard, row_guard(&rows[q]));
    assert_eq!(
        subject,
        "On a row with a progress bar, how should its primary button (e.g. Stop paste) look? \
         → Outlined on meters (Recommended)"
    );
    assert_eq!(unproven, "answered with its recommended option");
}

/// The recommendation wins over the vendor's focus: S2-01 marks option 2
/// with the focus on 1, so the focus moves one row down first.
#[test]
fn the_recommended_option_wins_over_the_focus() {
    let (_, d) = decided(f::QUESTION_RECOMMENDED_SECOND);
    let (steps, target, _, subject, _) = answer(&d);
    assert_eq!((steps, target), (1, AnswerTarget::Option(2)));
    assert!(subject.ends_with("→ Nextest (Recommended)"), "{subject}");
}

/// Nothing marked: option 1, and the journal says so.
#[test]
fn no_marked_option_answers_option_one() {
    let (_, d) = decided(f::QUESTION_NO_RECOMMENDATION);
    let (steps, target, _, subject, unproven) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)));
    assert!(subject.ends_with("→ Top"), "{subject}");
    assert_eq!(unproven, "no option is marked (Recommended): option 1");
}

/// A recommended label that wraps (S5-01 at 80 columns: `(Recommended)` on
/// the label's second row) is chosen, and the journal names the wrap.
/// Control: the same dialog at 180 columns is a plain `Label`.
#[test]
fn a_wrapped_recommended_label_is_chosen_and_said() {
    let (_, d) = decided(f::QUESTION_LONG_LABEL_80COL);
    let (steps, target, _, _, unproven) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)));
    assert!(
        unproven.starts_with("answered with its recommended option (its label wraps: read to row"),
        "{unproven}"
    );
    let (_, d) = decided(f::QUESTION_LONG_LABEL_180COL);
    assert_eq!(answer(&d).4, "answered with its recommended option");
}

/// The free-text row focused (S1-03, pristine): the focus is moved UP off it
/// to the recommended option, and the journal says it was moved. Never the
/// row itself.
#[test]
fn focus_on_the_free_text_row_is_moved_up_never_answered() {
    let (_, d) = decided(f::QUESTION_FREE_TEXT_FOCUSED);
    let (steps, target, _, _, unproven) = answer(&d);
    assert_eq!((steps, target), (-3, AnswerTarget::Option(1)));
    assert!(
        unproven.ends_with("; the focus was on the free-text row: moved off it first"),
        "{unproven}"
    );
}

/// The chat row focused (hand-built from S6-01): moved up to option 1.
#[test]
fn focus_on_the_chat_row_is_moved_up() {
    let (_, d) = decided(f::QUESTION_CHAT_FOCUSED);
    let (steps, target, _, _, unproven) = answer(&d);
    assert_eq!((steps, target), (-3, AnswerTarget::Option(1)));
    assert!(unproven.contains("the chat row"), "{unproven}");
}

/// R2 (d): text a person typed into the free-text row — focused or not, and
/// in a multi-select — is theirs: escalated, nothing keyed.
#[test]
fn a_persons_text_hands_the_question_over() {
    for text in [
        f::QUESTION_FREE_TEXT_TYPED,
        f::QUESTION_FREE_TEXT_TYPED_AWAY,
        f::QUESTION_MULTISELECT_TYPED,
    ] {
        let (_, d) = decided(text);
        assert!(
            reason(&d).starts_with("a question a person has begun answering (text in its"),
            "{d:?}"
        );
    }
}

/// NEVER `Type something.`, `Chat about this` or the review's cancel: over
/// every live and hand-built fixture aterm-phase reads whole, and every
/// variant with the `❯` moved onto each of its focusable rows, an answer's
/// Enter goes to one of the model's options, the multi-select button or
/// `Submit answers` — and a focus moved off a row counts the rows right.
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
            }
            let from = focus_position(&dialog, at).expect("in the order");
            let to = focus_position(&dialog, target_focus(target)).expect("in the order");
            assert_eq!(steps, to - from, "{text:.60} from {at:?}");
        }
    }
    assert!(answered > 60, "the sweep answered {answered} variants");
}

/// Multi-select: Enter toggles each recommended option in turn, then the
/// button moves on; the focus goes where each needs to. The pure decision
/// reads every check inside the recommended set as progress — S3-11's
/// checks were the capture script's, standing in for the supervisor's; the
/// loop, which remembers its own toggles, hands a check it did not make
/// over (`a_check_the_supervisor_did_not_make_hands_the_multiselect_over`).
#[test]
fn multiselect_toggles_each_recommended_then_advances() {
    let (_, d) = decided(f::QUESTION_MULTISELECT);
    let (steps, target, _, subject, _) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)));
    assert!(
        subject.ends_with("→ toggle Box drawing (Recommended)"),
        "{subject}"
    );
    let (_, d) = decided(f::QUESTION_MULTISELECT_CHECKED);
    let (steps, target, _, subject, _) = answer(&d);
    assert_eq!((steps, target), (2, AnswerTarget::Button));
    assert!(
        subject.ends_with("→ Next (Box drawing (Recommended), Powerline (Recommended))"),
        "{subject}"
    );
    let (_, d) = decided(f::QUESTION_MULTISELECT_NEXT);
    assert_eq!(
        (answer(&d).0, answer(&d).1),
        (0, AnswerTarget::Button),
        "{d:?}"
    );
}

/// A checked option the recommendation does not name is a person's check:
/// the question is theirs. Control: the unchecked dialog is answered.
#[test]
fn multiselect_with_a_foreign_check_hands_over() {
    let rows = screen(f::QUESTION_MULTISELECT);
    let edited: Vec<String> = rows
        .iter()
        .map(|r| r.replace("2. [ ] Emoji", "2. [✔] Emoji"))
        .collect();
    assert_ne!(rows, edited, "PRECONDITION: the edit landed");
    let d = decide_rows(&edited);
    assert!(
        reason(&d).contains("option 2 is checked, and it is not recommended"),
        "{d:?}"
    );
    assert!(matches!(decide_rows(&rows), Decision::Approve { .. }));
}

/// The preview form: the focus moved to the option, then Enter (a digit
/// only moves the focus there, S4-03/04). A revisited tab whose option is
/// ticked ` ✔` is a person's answer: escalated (R8, R2 (c)).
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

/// The review tab: Enter on `1. Submit answers` where the vendor put the
/// focus, guarded on the question row the dialog names (the submit row); a
/// review a person skipped to (`⚠ … not answered`) is submitted naming the
/// skipped headers; the focus on Cancel is moved up first.
#[test]
fn the_review_tab_submits() {
    let (rows, d) = decided(f::QUESTION_REVIEW);
    let (steps, target, guard, subject, _) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::ReviewSubmit));
    assert_eq!(
        guard,
        row_guard(&rows[row_of(&rows, "❯ 1. Submit answers")])
    );
    assert_eq!(subject, "submit answers (4 of 4 answered)");
    let (rows, d) = decided(f::QUESTION_REVIEW_UNANSWERED);
    let (steps, target, _, subject, unproven) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::ReviewSubmit));
    assert_eq!(subject, "submit answers (unanswered: Placement)");
    assert!(
        unproven.starts_with("a person moved past a question: submitted without its answer"),
        "{unproven}"
    );
    let cancel = with_focus(&rows, row_of(&rows, "2. Cancel"));
    let d = decide_rows(&cancel);
    assert_eq!(
        (answer(&d).0, answer(&d).1),
        (-1, AnswerTarget::ReviewSubmit)
    );
}

/// R10 under D2: a question with NO recommended option whose words name a
/// destructive act is answered only by its ONE refusing option (`No`), which
/// consents to nothing — option 1 of "Drop the table?" would be `Yes` — and
/// with none it is the person's; the same question with a marked
/// recommendation is answered (the owner's directive: the model's own
/// labelled option). Control: the measured Yes/No with nothing destructive
/// is answered with option 1. NEGATIVE CONTROLS: no refusing option, two of
/// them, and a "refusal" that names an act itself — each the person's.
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
    let (steps, target, _, subject, unproven) = answer(&refused);
    assert_eq!((steps, target), (1, AnswerTarget::Option(2)), "the one No");
    assert_eq!(subject, "Do you want to proceed? → No");
    assert!(unproven.contains("its one refusal"), "{unproven}");
    let marked: Vec<String> = drop
        .iter()
        .map(|r| r.replace("1. Yes", "1. Yes (Recommended)"))
        .collect();
    let (steps, target, ..) = answer(&decide_rows(&marked));
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)));
    // A refusal that names an act is no refusal; two refusals are no one.
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
            reason(&decide_rows(&edited))
                .ends_with("no one option that refuses it: a person answers that"),
            "{to}"
        );
    }
    // An inflected act on a DESCRIPTION row (the adversarial review of
    // 2026-09-25), with no option that refuses it: the person's.
    let rows = screen(f::QUESTION_NO_RECOMMENDATION);
    assert!(matches!(decide_rows(&rows), Decision::Approve { .. }));
    let drops: Vec<String> = rows
        .iter()
        .map(|r| r.replace("Above the tab strip.", "Drops the legacy table."))
        .collect();
    assert_ne!(rows, drops, "PRECONDITION: the edit landed");
    assert_eq!(
        reason(&decide_rows(&drops)),
        "a question with no recommended option that would drop, and no one option that refuses \
         it: a person answers that"
    );
}

/// The never-block design's incident (§4 of
/// `DESIGN-harness-never-block-2026-09-24.md`): `Want me to publish this
/// now?` with `Deploy and push` / `Push only` / `Hold everything` and no
/// `(Recommended)`. Option 1 is NOT pressed — the question names an act and
/// no option refuses it (`Hold everything` does not start `No`), so it is the
/// person's. Control: the same labels on a question that names nothing are
/// answered; and with `Hold everything` spelled `No, hold everything` that
/// one refusal is the answer.
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
    assert_eq!(
        reason(&decide_rows(&rows)),
        "a question with no recommended option that would deploy, and no one option that \
         refuses it: a person answers that"
    );
    // Control: the labels alone name no act.
    let neutral: Vec<String> = rows
        .iter()
        .map(|r| {
            r.replace("Want me to publish this now?", "Which should come first?")
                .replace("1. Deploy and push", "1. Tests and docs")
        })
        .collect();
    assert_eq!(answer(&decide_rows(&neutral)).1, AnswerTarget::Option(1));
    // With one option that refuses, that option is the answer.
    let refused = incident("3. No, hold everything");
    assert_eq!(answer(&decide_rows(&refused)).1, AnswerTarget::Option(3));
}

/// D2's refusal is `No` itself or `No` then a separator — never a word that
/// merely starts with it.
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

/// D2 for Codex's question (by its answers' roles, its digit): an unmarked
/// question that would delete is answered by its one `No` answer, and one
/// with no refusing answer is the person's.
#[test]
fn codexs_unmarked_destructive_question_is_answered_by_its_refusal() {
    let rows = screen(aterm_phase::codex::fixtures::QUESTION);
    let edit = |rows: &[String], pairs: &[(&str, &str)]| -> Vec<String> {
        rows.iter()
            .map(|r| {
                pairs
                    .iter()
                    .fold(r.clone(), |r, (from, to)| r.replace(from, to))
            })
            .collect()
    };
    let asked = [
        (
            "What should the new subtraction function be called?",
            "Should I delete the old calc.py first?",
        ),
        ("1. subtract (Recommended)", "1. Yes                    "),
        ("2. sub                    ", "2. No                     "),
    ];
    let q = edit(&rows, &asked);
    assert_ne!(q, rows, "PRECONDITION: the edits landed");
    let reading = aterm_phase::read(Some("codex"), &q, None);
    let p = reading.prompt.as_ref().expect("the dialog");
    match answer_question(p, &q) {
        Decision::Approve {
            choice, unproven, ..
        } => {
            assert_eq!(choice, Choice::Digit(2), "the one No");
            assert!(
                unproven
                    .as_deref()
                    .is_some_and(|u| u.contains("its one refusal")),
                "{unproven:?}"
            );
        }
        other => panic!("not answered: {other:?}"),
    }
    // NEGATIVE CONTROL: no answer refuses it.
    let q = edit(&rows, &asked[..2]);
    let reading = aterm_phase::read(Some("codex"), &q, None);
    let p = reading.prompt.as_ref().expect("the dialog");
    assert!(
        reason(&answer_question(p, &q)).contains("no one option that refuses it"),
        "{q:#?}"
    );
}

/// What aterm-phase did not read whole is never answered: the column-1
/// shapes 2.1.282 does not draw, a scrolled list, the dialog withheld
/// behind a draft in the composer (R5).
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
        let d = answer_question(&p, &rows);
        assert!(
            reason(&d).starts_with("a question (AskUserQuestion) aterm-phase did not read whole"),
            "{d:?}"
        );
    }
    let rows = screen(f::QUESTION_DEFERRED);
    let p = parse_prompt_v2(&rows).expect("a box");
    assert!(
        reason(&answer_question(&p, &rows)).ends_with("withheld behind a draft in the composer"),
        "the badge names the draft"
    );
}

fn ctx(approve: Approve, answer_questions: bool) -> ApprovalCtx {
    let mut c = ApprovalCtx::new(PathBuf::from("/tmp/work"), None, 501, None);
    c.approve = approve;
    c.answer_questions = answer_questions;
    c
}

fn decide_with(text: &str, approve: Approve, answer_questions: bool) -> Decision {
    let rows = screen(text);
    let reading = aterm_phase::read(Some("claude"), &rows, None);
    decide(&reading, &rows, &ctx(approve, answer_questions))
}

/// `answer_questions = false` hands every question over, whatever `approve`
/// says; on, questions are answered whatever `approve` says — a question is
/// no permission, and `approve` limits permission boxes only (D5).
#[test]
fn answer_questions_off_hands_every_question_over() {
    for approve in [Approve::All, Approve::Safe, Approve::None] {
        let d = decide_with(f::QUESTION_TABS_INCIDENT, approve, false);
        assert_eq!(
            reason(&d),
            "answer_questions is off: a question is the person's to answer",
            "{approve:?}"
        );
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
    // NEGATIVE CONTROL: full power still never answers one by itself.
    let d = decide_with(f::QUESTION_TABS_INCIDENT, Approve::All, false);
    assert!(matches!(d, Decision::Escalate { .. }), "{d:?}");
}

/// A question whose head the pane cut is escalated: aterm-phase reads it with
/// its head off the screen, never whole, and the question answer hands it
/// over naming that.
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
        reason(&d).starts_with("a question (AskUserQuestion) aterm-phase did not read whole"),
        "{d:?}"
    );
}

/// R11: no visible focus is no answer. aterm-phase reads such a dialog as
/// unknown; the decider refuses it whichever way it arrives.
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

/// The focus is never moved further than [`MAX_QUESTION_FOCUS_STEPS`] rows.
#[test]
fn the_step_cap_is_the_furthest_measured_move_plus_one() {
    assert_eq!(MAX_QUESTION_FOCUS_STEPS, 6);
    // Option 1 to `Next` in this three-option multi-select: four rows.
    let rows = screen(f::QUESTION_MULTISELECT_CHECKED);
    let one = row_of(&rows, "1. [✔] Box drawing (Recommended)");
    let d = decide_rows(&with_focus(&rows, one));
    assert_eq!((answer(&d).0, answer(&d).1), (4, AnswerTarget::Button));
}

// ---- Tier-1: the decider bound to `SupervisorQuestionAnswer` ------------

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

/// What the model lets the loop do at `st` once the loop's own guards hold
/// (a person quiet, no person key since its read, its last key taken, the
/// queue empty): its DECISION
/// part, which is the pure decider's.
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

/// The dialog a model read stands for, drawn from the live captures of the
/// incident's last two tabs (S6-03, S6-04: two options, the first
/// recommended, the free-text row 3, the chat row 4) and its review
/// (S6-05): `focus` 0 the `❯` on the chosen row (option 1, `1. Submit
/// answers`), 1 elsewhere — on a question tab the free-text row at `row`
/// parity 0, option 2 (or, `on_chat`, the chat row) at parity 1; on the
/// review its cancel — and `typed` a person's `2` in the free-text row.
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

/// TIER-1 for `SupervisorQuestionAnswer` (aterm-spec
/// `supervisor_question_answer_model`), the DECISION: at every reachable
/// state of the model where the loop holds a fresh read of the dialog, the
/// real [`answer_question`] — over the live captures drawn as that read
/// showed them ([`drawn`]) — presses Enter exactly where the model's
/// `HarnessEnter` is enabled, moves the focus up exactly where `HarnessMove`
/// is, and hands the question over everywhere else (a person's text in the
/// free-text row), the loop's own guards granted ([`decision_of`]). Every
/// verdict occurs, on the question tabs and on the review. NEGATIVE
/// CONTROL: the decider before R2 (d) — Enter on the chosen row whatever
/// the free-text row holds — is caught by this very comparison at the
/// typed-away state, where the real one escalates.
#[test]
fn tier1_the_question_decider_chooses_only_what_the_model_enables() {
    let model = aterm_spec::derive::supervisor_question_answer_model();
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
    // Negative control: the typed-away read (a person's `2` in the field,
    // the focus back on option 1). The model and the real decider hand it
    // over; a decider blind to the free-text row's text (the one before R2
    // (d): it decides as on the pristine rows) presses Enter there — and the
    // comparison above tells the two apart.
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
