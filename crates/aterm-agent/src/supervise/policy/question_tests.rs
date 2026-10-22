// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The question answer over aterm-phase's fixtures of Claude Code's question
//! dialog (live captures and hand-built shapes): which row is chosen, how the
//! focus gets there, and every shape that is the person's.

use std::path::PathBuf;

use aterm_phase::prompt::fixtures::{self as f, screen};
use aterm_phase::{PromptKind, parse_prompt_v2};

use super::super::super::config::Approve;
use super::super::approval::{
    Answer, AnswerTarget, ApprovalCtx, Choice, Decision, QuestionPriority, decide,
};
use super::super::guard::row_guard;
use super::*;

/// The decision under [`QuestionPriority::Recommended`], the pane as wide as
/// its widest row.
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

/// A fixture's rows and the decision on its question.
fn decided(text: &str) -> (Vec<String>, Decision) {
    let rows = screen(text);
    let p = parse_prompt_v2(&rows).unwrap_or_else(|| panic!("no box: {rows:#?}"));
    assert_eq!(p.kind, PromptKind::Question, "{rows:#?}");
    let d = recommended_answer(&p, &rows);
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

/// The decision on edited rows.
fn decide_rows(rows: &[String]) -> Decision {
    let p = parse_prompt_v2(rows).unwrap_or_else(|| panic!("no box: {rows:#?}"));
    recommended_answer(&p, rows)
}

/// A recorded screen (S6-01) is answered with its recommended option by one
/// Enter where the vendor put the focus, guarded on the question's first row
/// as drawn; the ledger names the question and the label.
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

/// The recommendation wins over the vendor's focus: S2-01 marks option 2
/// with the focus on 1, so the focus moves one row down first.
#[test]
fn the_recommended_option_wins_over_the_focus() {
    let (_, d) = decided(f::QUESTION_RECOMMENDED_SECOND);
    let (steps, target, _, _, _) = answer(&d);
    assert_eq!((steps, target), (1, AnswerTarget::Option(2)));
}

/// Nothing marked: option 1, and the journal says so.
#[test]
fn no_marked_option_answers_option_one() {
    let (_, d) = decided(f::QUESTION_NO_RECOMMENDATION);
    let (steps, target, _, _, _) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)));
}

/// A vendor's answer kept for good with no KEPT word (`Enable selected`, the
/// MCP server dialog for several's submit) is an option's WHOLE label
/// ([`aterm_phase::prompt::kept_answer`]), never words in it: an agent's
/// `Enable selected suites (Recommended)`, described `Enable selected suites
/// in parallel.`, is answered. NEGATIVE CONTROL: an unmarked question whose
/// option 1 is exactly `Enable selected` is a person's.
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
    // The one-server dialog's `… all future …` answer is kept by its whole
    // label, not by its words: an agent's option saying them is answered.
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

/// A security alert's model switch stays on the current model (owner ruling):
/// Claude Code's `Model switch` dialog, raised because the
/// model's safeguards flagged a message, is answered with its `Stay on <model>`
/// option — never auto-downgraded — instead of escalated. Its `1. Switch
/// automatically` option would otherwise be `kept` for a person ([`kept`]).
#[test]
fn the_security_model_switch_stays_on_the_current_model() {
    let (_, d) = decided(f::QUESTION_MODEL_SWITCH_PREFERENCE);
    let (steps, target, _, subject, unproven) = answer(&d);
    // The fixture opens with the focus on option 1, so Stay (option 2) is one
    // row down.
    assert_eq!((steps, target), (1, AnswerTarget::Option(2)));
    assert!(subject.ends_with("→ Stay on Opus 5.5"), "{subject}");
    assert_eq!(
        unproven,
        "a security alert's model switch: stayed on the current model (owner ruling, 2026-10-16)"
    );
}

/// The CONTROL: the same dialog with the alert's own words gone is no longer
/// that dialog, so the stay rule does not fire and `1. Switch automatically`
/// escalates to a person as before. This is the rule's scope: it answers only
/// a safeguards alert, never another model-switch or preference question.
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

/// The SECOND control, for the companion requirement: the alert's words are
/// kept, but its `1. Switch automatically` auto-downgrade option is gone —
/// replaced by a plain one-off row with no `switch` and no `settles_later`
/// grant, as a coincidental agent-posed `AskUserQuestion` that merely shares
/// the `safeguards`/`flagged` wording and a lone `Stay on …` option would
/// look. The stay rule keys on the real dialog's WHOLE shape — the
/// `Stay on …` row paired with that settling `Switch automatically` sibling —
/// so without the companion it does not fire: the question falls through to
/// the baseline policy, which presses option 1 (nothing marked), never this
/// rule's `Stay` press. This closes the silence-reads-as-success hole where a
/// look-alike question was answered on the agent's behalf.
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
    // Baseline, not the stay rule: the focused first option, not the `Stay`
    // option (option 2), and the baseline's journal line.
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)), "{d:?}");
    assert_eq!(unproven, "no option is marked (Recommended): option 1");
}

/// The THIRD control, for the destructive guard: the full genuine shape is
/// kept (safeguards+flagged, the `Stay on …` option, the `Switch automatically`
/// companion) but the question now names a destructive act — as an agent-posed
/// look-alike can, since the agent authors every word and an `AskUserQuestion`
/// renders through this same dialog. The stay rule must NOT auto-press: it
/// returns `None` so the dialog reaches the baseline policy, whose
/// destructive-word rule escalates a no-recommendation destructive question to
/// a person. WITHOUT the guard this look-alike would be silently auto-stayed,
/// skipping that human escalation. The genuine dialog names no destructive
/// word, so its Stay press (the test above) is unaffected.
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

/// A recommended label that wraps (S5-01 at 80 columns: `(Recommended)` on
/// the label's second row) is chosen.
#[test]
fn a_wrapped_recommended_label_is_chosen() {
    let (_, d) = decided(f::QUESTION_LONG_LABEL_80COL);
    let (steps, target, ..) = answer(&d);
    assert_eq!((steps, target), (0, AnswerTarget::Option(1)));
}

/// The free-text row focused (S1-03, pristine): the focus is moved up off it
/// to the recommended option, and the journal says it was moved. Never the
/// row itself.
#[test]
fn focus_on_the_free_text_row_is_moved_up_never_answered() {
    let (_, d) = decided(f::QUESTION_FREE_TEXT_FOCUSED);
    let (steps, target, ..) = answer(&d);
    assert_eq!((steps, target), (-3, AnswerTarget::Option(1)));
}

/// The chat row focused (hand-built from S6-01): moved up to option 1.
#[test]
fn focus_on_the_chat_row_is_moved_up() {
    let (_, d) = decided(f::QUESTION_CHAT_FOCUSED);
    let (steps, target, ..) = answer(&d);
    assert_eq!((steps, target), (-3, AnswerTarget::Option(1)));
}

/// Text a person typed into the free-text row — focused or not, and
/// in a multi-select — is theirs: escalated, nothing keyed.
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

/// Never `Type something.`, `Chat about this` or the review's cancel: over
/// every fixture aterm-phase reads whole, and every variant with the `❯` moved
/// onto each focusable row, an answer's Enter goes to one of the model's
/// options, the multi-select button or `Submit answers`, and a focus moved
/// off a row counts the rows right.
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

/// Multi-select: Enter toggles each recommended option in turn, then the
/// button moves on; the focus goes where each needs to. The pure decision
/// reads every check inside the recommended set as progress; the loop, which
/// remembers its own toggles, hands a check it did not make over
/// (`a_check_the_supervisor_did_not_make_hands_the_multiselect_over`).
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
    assert!(reason(&d).contains("option 2 is checked"), "{d:?}");
    assert!(matches!(decide_rows(&rows), Decision::Approve { .. }));
}

/// The preview form: the focus moved to the option, then Enter (a digit
/// only moves the focus there, S4-03/04). A revisited tab whose option is
/// ticked ` ✔` is a person's answer: escalated.
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

/// An unmarked question naming a destructive act is answered only by its one
/// refusing option (`No`), else it is the person's; with a marked
/// recommendation it is answered. Controls: a Yes/No naming nothing takes
/// option 1; no refusal, two, or a "refusal" that names an act are the
/// person's.
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
            reason(&decide_rows(&edited)).contains("no one option that refuses it"),
            "{to}"
        );
    }
    // An inflected act on a description row, with no option that refuses it:
    // the person's.
    let rows = screen(f::QUESTION_NO_RECOMMENDATION);
    assert!(matches!(decide_rows(&rows), Decision::Approve { .. }));
    let drops: Vec<String> = rows
        .iter()
        .map(|r| r.replace("Above the tab strip.", "Drops the legacy table."))
        .collect();
    assert_ne!(rows, drops, "PRECONDITION: the edit landed");
    assert!(reason(&decide_rows(&drops)).contains("would drop"));
}

/// An unmarked question naming an act with no option that refuses it (`Hold
/// everything` does not start `No`) is the person's. Control: the same labels
/// on a question that names nothing are answered; with `No, hold everything`
/// that one refusal is the answer.
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
    assert!(reason(&decide_rows(&rows)).contains("would deploy"));
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

/// The refusal is `No` itself or `No` then a separator — never a word that
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

/// Muse Code's question (measured on 1.4.10, chosen by the cursor and Enter,
/// a digit only moving its cursor) is answered by the focus moved to its
/// `(Recommended)` answer and Enter on it ([`Choice::Focus`]), guarded on
/// the question's own row, never its head (whose clock ticks): from the
/// first row it sits on, from `Blue` one up, from `None of the above` two up
/// — never Enter on the person's own words. Unmarked, its first answer; an
/// unmarked one that would delete, its one `No`; under `long-term` the
/// word's text as its note ([`Choice::Note`], guarded on the question's own
/// row; `muses_question_takes_the_text_as_a_note_and_its_review_is_sent`).
/// NEGATIVE CONTROLS: the guard matches no head row; a cursor on no option is escalated (its
/// reader gives such options no role).
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

/// What aterm-phase did not read whole is never answered: the column-1
/// shapes 2.1.282 does not draw, a scrolled list, the dialog withheld
/// behind a draft in the composer.
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

/// `answer_questions = false` hands every question over, whatever `approve`
/// says; on, questions are answered whatever `approve` says — a question is
/// no permission, and `approve` limits permission boxes only.
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
    // Negative control: full power still never answers one by itself.
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
        reason(&d).starts_with("a question aterm could not read whole"),
        "{d:?}"
    );
}

/// No visible focus is no answer. aterm-phase reads such a dialog as
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
/// queue empty): its decision part, which is the pure decider's.
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

/// The dialog a model read stands for, drawn from live captures (S6-03,
/// S6-04: two options, the first recommended, the free-text row 3, the chat
/// row 4) and a review (S6-05): `focus` 0 the `❯` on the chosen row, 1
/// elsewhere (on a question tab the free-text row at `row` parity 0, option 2
/// or, `on_chat`, the chat row at parity 1; on the review its cancel), and
/// `typed` a person's `2` in the free-text row.
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

/// Tier-1 for `SupervisorQuestionAnswer` (aterm-spec
/// `supervisor_question_answer_model`): at every reachable state holding a
/// fresh read, the real [`answer_question`] over [`drawn`] presses Enter
/// exactly where `HarnessEnter` is enabled, moves the focus up exactly where
/// `HarnessMove` is, and hands the question over elsewhere ([`decision_of`]).
/// Negative control: the typed-away state.
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
    // Negative control: the typed-away read (a person's `2` in the field, the
    // focus back on option 1), which a decider blind to the field's text
    // would press Enter on.
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
