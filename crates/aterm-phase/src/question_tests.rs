// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The question dialog as Claude Code 2.1.282 draws it (module header of
//! `question.rs`): every live capture of 2026-09-25 read as its form, the
//! hand-built shapes the critique of that day named (R5–R13), and the
//! negative controls that keep a quoted, cut or unsound dialog from ever
//! being read whole — or read idle.

use super::*;
use crate::phase::{Phase, worker_phase};
use crate::prompt::fixtures::*;
use crate::prompt::{PromptKind, PromptV2, Role, Select, parse_prompt_v2};

/// A fixture's rows, its box and its dialog.
fn read(text: &str) -> (Vec<String>, PromptV2, QuestionDialog) {
    let r = screen(text);
    let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("no box: {r:#?}"));
    let d = p.question_dialog.clone().expect("a question's dialog");
    (r, p, d)
}

/// The row whose trimmed text is `text`.
fn row_of(r: &[String], text: &str) -> usize {
    r.iter()
        .position(|x| x.trim() == text)
        .unwrap_or_else(|| panic!("no row {text:?}"))
}

fn options(d: &QuestionDialog) -> &[QuestionOption] {
    match &d.form {
        QuestionForm::Single { options, .. }
        | QuestionForm::Multi { options, .. }
        | QuestionForm::Preview { options, .. } => options,
        other => panic!("no options: {other:?}"),
    }
}

fn free_row(d: &QuestionDialog) -> &FreeText {
    match &d.form {
        QuestionForm::Single { free_text, .. } | QuestionForm::Multi { free_text, .. } => free_text,
        other => panic!("no free-text row: {other:?}"),
    }
}

fn why(d: &QuestionDialog) -> &'static str {
    match d.form {
        QuestionForm::Unknown { why } => why,
        ref other => panic!("read whole: {other:?}"),
    }
}

/// F1: the incident's screen (S6-01, the live replica of the owner's
/// 2026-09-25 screen) reads whole — where the walk up from the footer had
/// stopped at the rule over `4. Chat about this`, a three-row box with no
/// question and no option (kind `other` on 0.93.0).
#[test]
fn the_incident_question_reads_whole() {
    let (r, p, d) = read(QUESTION_TABS_INCIDENT);
    let bar = r
        .iter()
        .position(|x| x.starts_with('←'))
        .expect("the tab bar");
    let footer = r.len() - 1;
    assert_eq!(p.kind, PromptKind::Question);
    assert!(!p.head_off_screen);
    assert_eq!(p.span, (bar, footer));
    assert_eq!(p.title, r[bar].trim());
    let headers: Vec<(&str, bool)> = d
        .tabs
        .iter()
        .map(|t| (t.header.as_str(), t.answered))
        .collect();
    assert_eq!(
        headers,
        [
            ("Button", false),
            ("Grey cursor", false),
            ("Glyphs", false),
            ("Placement", false)
        ]
    );
    assert!(d.submit_tab);
    assert_eq!(d.width, 180);
    assert_eq!(d.auto_continue, None);
    let QuestionForm::Single {
        question,
        options,
        free_text,
        chat,
    } = &d.form
    else {
        panic!("{:?}", d.form)
    };
    assert_eq!(
        question.text,
        "On a row with a progress bar, how should its primary button (e.g. Stop paste) look?"
    );
    assert!(question.gutter);
    assert_eq!(question.rows, bar + 2..bar + 3);
    let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Outlined on meters (Recommended)", "Solid everywhere"]
    );
    assert!(
        options[0]
            .description
            .starts_with("An accent ring with no solid fill")
    );
    assert_eq!(
        options[1].description,
        "Keep today's solid accent button on every row, including over a progress bar."
    );
    let rec: Vec<Recommended> = options.iter().map(|o| o.recommended).collect();
    assert_eq!(rec, [Recommended::Label, Recommended::No]);
    assert_eq!(d.focus(), QuestionFocus::Option(1));
    assert_eq!((free_text.n, free_text.pristine), (3, true));
    assert_eq!(free_text.text, "Type something.");
    assert_eq!((chat.n, chat.focused), (Some(4), false));
    assert_eq!(p.select, Select::Digits);
    assert!(p.options.iter().all(|o| o.role == Role::Other));
    let opt_labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        opt_labels,
        [
            "Outlined on meters (Recommended)",
            "Solid everywhere",
            "Type something.",
            "Chat about this"
        ]
    );
    assert_eq!(p.question, None);
    assert_eq!(p.description, question.text);
    assert_eq!(d.guard_row(), Some(bar + 2));
    assert!(r[bar + 2].starts_with("│ On a row"));
    let reading = crate::reader::read(Some("claude"), &r, None);
    assert_eq!(reading.phase, Phase::Prompt);
    assert!(reading.phase_authoritative);
}

/// One live capture's expected reading.
struct Live<'a> {
    name: &'static str,
    text: &'static str,
    form: &'static str,
    focus: QuestionFocus,
    recommended: &'a [Recommended],
    checked: &'static [Option<bool>],
    /// The free-text row's `pristine`, where the form has one.
    pristine: Option<bool>,
    answered: &'static [bool],
    submit_tab: bool,
    unanswered: bool,
}

/// F1: the table over every live capture of 2026-09-25 — its form, its
/// focus, its options' `(Recommended)` and checkboxes, its free-text row,
/// its tabs — each read whole, prompt phase, authoritative, every role
/// `Other`.
#[test]
fn every_live_question_capture_reads_as_its_form() {
    use QuestionFocus as F;
    use Recommended::{Label, No};
    const Y: bool = true;
    const N: bool = false;
    let wrapped = |text: &str| {
        let r = screen(text);
        let at = r
            .iter()
            .position(|x| x.trim() == "meter (Recommended)")
            .expect("the wrap");
        Recommended::Wrapped { label_end: at }
    };
    let w80 = [wrapped(QUESTION_LONG_LABEL_80COL), No, No];
    let table: Vec<Live<'_>> = vec![
        Live {
            name: "tabs-incident",
            text: QUESTION_TABS_INCIDENT,
            form: "single",
            focus: F::Option(1),
            recommended: &[Label, No],
            checked: &[None, None],
            pristine: Some(true),
            answered: &[N, N, N, N],
            submit_tab: true,
            unanswered: false,
        },
        Live {
            name: "tabs-second",
            text: QUESTION_TABS_SECOND,
            form: "single",
            focus: F::Option(1),
            recommended: &[Label, No],
            checked: &[None, None],
            pristine: Some(true),
            answered: &[Y, N, N, N],
            submit_tab: true,
            unanswered: false,
        },
        Live {
            name: "tabs-third",
            text: QUESTION_TABS_THIRD,
            form: "single",
            focus: F::Option(1),
            recommended: &[Label, No],
            checked: &[None, None],
            pristine: Some(true),
            answered: &[Y, Y, N, N],
            submit_tab: true,
            unanswered: false,
        },
        Live {
            name: "tabs-fourth",
            text: QUESTION_TABS_FOURTH,
            form: "single",
            focus: F::Option(1),
            recommended: &[Label, No],
            checked: &[None, None],
            pristine: Some(true),
            answered: &[Y, Y, Y, N],
            submit_tab: true,
            unanswered: false,
        },
        Live {
            name: "review",
            text: QUESTION_REVIEW,
            form: "review",
            focus: F::ReviewSubmit,
            recommended: &[],
            checked: &[],
            pristine: None,
            answered: &[Y, Y, Y, Y],
            submit_tab: true,
            unanswered: false,
        },
        Live {
            name: "review-unanswered",
            text: QUESTION_REVIEW_UNANSWERED,
            form: "review",
            focus: F::ReviewSubmit,
            recommended: &[],
            checked: &[],
            pristine: None,
            answered: &[Y, Y, Y, N],
            submit_tab: true,
            unanswered: true,
        },
        Live {
            name: "one",
            text: QUESTION_ONE,
            form: "single",
            focus: F::Option(1),
            recommended: &[Label, No, No],
            checked: &[None, None, None],
            pristine: Some(true),
            answered: &[N],
            submit_tab: false,
            unanswered: false,
        },
        Live {
            name: "recommended-second",
            text: QUESTION_RECOMMENDED_SECOND,
            form: "single",
            focus: F::Option(1),
            recommended: &[No, Label, No],
            checked: &[None, None, None],
            pristine: Some(true),
            answered: &[N],
            submit_tab: false,
            unanswered: false,
        },
        Live {
            name: "free-text-focused",
            text: QUESTION_FREE_TEXT_FOCUSED,
            form: "single",
            focus: F::FreeText,
            recommended: &[Label, No, No],
            checked: &[None, None, None],
            pristine: Some(true),
            answered: &[N],
            submit_tab: false,
            unanswered: false,
        },
        Live {
            name: "free-text-typed",
            text: QUESTION_FREE_TEXT_TYPED,
            form: "single",
            focus: F::FreeText,
            recommended: &[Label, No, No],
            checked: &[None, None, None],
            pristine: Some(false),
            answered: &[N],
            submit_tab: false,
            unanswered: false,
        },
        Live {
            name: "free-text-typed-away",
            text: QUESTION_FREE_TEXT_TYPED_AWAY,
            form: "single",
            focus: F::Option(3),
            recommended: &[Label, No, No],
            checked: &[None, None, None],
            pristine: Some(false),
            answered: &[N],
            submit_tab: false,
            unanswered: false,
        },
        Live {
            name: "multiselect",
            text: QUESTION_MULTISELECT,
            form: "multi",
            focus: F::Option(1),
            recommended: &[Label, No, Label],
            checked: &[Some(N), Some(N), Some(N)],
            pristine: Some(true),
            answered: &[Y, Y, N, N],
            submit_tab: true,
            unanswered: false,
        },
        Live {
            name: "multiselect-checked",
            text: QUESTION_MULTISELECT_CHECKED,
            form: "multi",
            focus: F::Option(3),
            recommended: &[Label, No, Label],
            checked: &[Some(Y), Some(N), Some(Y)],
            pristine: Some(true),
            answered: &[Y, Y, Y, N],
            submit_tab: true,
            unanswered: false,
        },
        Live {
            name: "multiselect-next",
            text: QUESTION_MULTISELECT_NEXT,
            form: "multi",
            focus: F::Button,
            recommended: &[Label, No, Label],
            checked: &[Some(Y), Some(N), Some(Y)],
            pristine: Some(true),
            answered: &[Y, Y, Y, N],
            submit_tab: true,
            unanswered: false,
        },
        Live {
            name: "multiselect-typed",
            text: QUESTION_MULTISELECT_TYPED,
            form: "multi",
            focus: F::FreeText,
            recommended: &[Label, No, Label],
            checked: &[Some(Y), Some(N), Some(Y)],
            pristine: Some(false),
            answered: &[Y, Y, Y, N],
            submit_tab: true,
            unanswered: false,
        },
        Live {
            name: "no-recommendation",
            text: QUESTION_NO_RECOMMENDATION,
            form: "single",
            focus: F::Option(1),
            recommended: &[No, No, No],
            checked: &[None, None, None],
            pristine: Some(true),
            answered: &[Y, Y, Y, N],
            submit_tab: true,
            unanswered: false,
        },
        Live {
            name: "preview",
            text: QUESTION_PREVIEW,
            form: "preview",
            focus: F::Option(1),
            recommended: &[Label, No, No],
            checked: &[None, None, None],
            pristine: None,
            answered: &[N],
            submit_tab: false,
            unanswered: false,
        },
        Live {
            name: "long-label-80col",
            text: QUESTION_LONG_LABEL_80COL,
            form: "single",
            focus: F::Option(1),
            recommended: &w80,
            checked: &[None, None, None],
            pristine: Some(true),
            answered: &[N],
            submit_tab: false,
            unanswered: false,
        },
        Live {
            name: "long-label-180col",
            text: QUESTION_LONG_LABEL_180COL,
            form: "single",
            focus: F::Option(1),
            recommended: &[Label, No, No],
            checked: &[None, None, None],
            pristine: Some(true),
            answered: &[N],
            submit_tab: false,
            unanswered: false,
        },
    ];
    assert_eq!(table.len(), 19, "every live dialog capture");
    for want in &table {
        let name = want.name;
        let (r, p, d) = read(want.text);
        assert_eq!(p.kind, PromptKind::Question, "{name}");
        assert!(!p.head_off_screen, "{name}");
        assert!(p.options.iter().all(|o| o.role == Role::Other), "{name}");
        let form = match &d.form {
            QuestionForm::Single { .. } => "single",
            QuestionForm::Multi { .. } => "multi",
            QuestionForm::Preview { .. } => "preview",
            QuestionForm::Review { .. } => "review",
            QuestionForm::Unknown { why } => panic!("{name}: not read whole: {why}"),
        };
        assert_eq!(form, want.form, "{name}");
        assert_eq!(d.focus(), want.focus, "{name}");
        let answered: Vec<bool> = d.tabs.iter().map(|t| t.answered).collect();
        assert_eq!(answered, want.answered, "{name}");
        assert_eq!(d.submit_tab, want.submit_tab, "{name}");
        if let QuestionForm::Review {
            unanswered,
            submit,
            cancel,
        } = &d.form
        {
            assert_eq!(*unanswered, want.unanswered, "{name}");
            assert_eq!(r[submit.row].trim(), "❯ 1. Submit answers", "{name}");
            assert_eq!(r[cancel.row].trim(), "2. Cancel", "{name}");
            assert_eq!(d.guard_row(), Some(submit.row), "{name}");
            assert_eq!(d.question(), None, "{name}");
            let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
            assert_eq!(labels, ["Submit answers", "Cancel"], "{name}");
            assert_eq!(p.span.1, cancel.row, "{name}");
            continue;
        }
        let o = options(&d);
        let rec: Vec<Recommended> = o.iter().map(|o| o.recommended).collect();
        assert_eq!(rec, want.recommended, "{name}");
        let checked: Vec<Option<bool>> = o.iter().map(|o| o.checked).collect();
        assert_eq!(checked, want.checked, "{name}");
        assert!(o.iter().all(|o| !o.selected), "{name}");
        let numbers: Vec<u8> = o.iter().map(|o| o.n).collect();
        let want_numbers: Vec<u8> = (1..=o.len() as u8).collect();
        assert_eq!(numbers, want_numbers, "{name}");
        match want.pristine {
            Some(pristine) => assert_eq!(free_row(&d).pristine, pristine, "{name}"),
            None => assert!(matches!(d.form, QuestionForm::Preview { .. }), "{name}"),
        }
        let q = d.question().expect("the question");
        assert!(!q.text.is_empty() && !q.text.contains('│'), "{name}");
        assert_eq!(d.guard_row(), Some(q.rows.start), "{name}");
        assert_eq!(
            p.span.0 + 2,
            q.rows.start,
            "{name}: tab bar, blank, question"
        );
        let reading = crate::reader::read(Some("claude"), &r, None);
        assert_eq!(reading.phase, Phase::Prompt, "{name}");
        assert!(reading.phase_authoritative, "{name}");
    }
}

/// F1: a bare question at column 0 (S6-02) is inside the box, not a shell's
/// line that ends it; the chat row's `❯` at column 0 is the dialog's focus,
/// not the user's row (it read authoritative idle: the walk stopped there).
/// NEGATIVE CONTROL: the same rows with Claude's composer under the footer
/// — a copy in the transcript — are no box.
#[test]
fn the_question_zone_is_not_a_shell_line_nor_the_option_cursor_the_users_row() {
    let (r, p, d) = read(QUESTION_TABS_SECOND);
    let q = d.question().expect("the question");
    assert!(!q.gutter);
    assert_eq!(q.text, "Should the idle cursor fade to grey?");
    assert!(p.span.0 < q.rows.start);
    assert_eq!(worker_phase(&r), Phase::Prompt);
    let (r, p, d) = read(QUESTION_CHAT_FOCUSED);
    assert_eq!(p.kind, PromptKind::Question);
    assert_eq!(d.focus(), QuestionFocus::Chat);
    assert!(!p.head_off_screen);
    assert_eq!(worker_phase(&r), Phase::Prompt);
    for text in [
        QUESTION_TABS_SECOND,
        QUESTION_CHAT_FOCUSED,
        QUESTION_TABS_INCIDENT,
    ] {
        let mut quoted = screen(text);
        quoted.push(String::new());
        quoted.extend(composer("  ? for shortcuts"));
        assert_eq!(parse_prompt_v2(&quoted), None, "{quoted:#?}");
        assert_ne!(worker_phase(&quoted), Phase::Prompt);
    }
}

/// F1: the Submit (review) tab, which draws no footer, is the question
/// dialog — it read authoritative idle (S6-05, S3-17). NEGATIVE CONTROLS:
/// without `Review your answers` it is a question with its head off the
/// screen, form unknown — escalated, never idle, never the review; the same
/// rows over a composer (quoted in the transcript) are no box.
#[test]
fn the_review_tab_is_a_question() {
    for (text, unanswered) in [(QUESTION_REVIEW, false), (QUESTION_REVIEW_UNANSWERED, true)] {
        let (r, p, d) = read(text);
        let QuestionForm::Review {
            unanswered: u,
            submit,
            cancel,
        } = d.form
        else {
            panic!("{:?}", d.form)
        };
        assert_eq!(u, unanswered);
        assert_eq!(cancel.row, submit.row + 1);
        assert!(submit.focused && !cancel.focused);
        assert_eq!(d.focus(), QuestionFocus::ReviewSubmit);
        assert_eq!(d.tabs.len(), 4);
        assert_eq!(d.tabs.iter().all(|t| t.answered), !unanswered);
        assert_eq!(d.width, 180);
        assert_eq!(p.select, Select::Digits);
        assert_eq!(worker_phase(&r), Phase::Prompt);
        assert!(!crate::prompt::footed(&r, &p));
        // Its cancel focused: still the review.
        let mut moved = r.clone();
        moved[submit.row] = "  1. Submit answers".to_string();
        moved[cancel.row] = "❯ 2. Cancel".to_string();
        let d = parse_prompt_v2(&moved)
            .and_then(|p| p.question_dialog)
            .expect("the review");
        assert_eq!(d.focus(), QuestionFocus::ReviewCancel);
        // No title: read with its head off the screen, never the review.
        let mut untitled = r.clone();
        let title = row_of(&r, "Review your answers");
        untitled[title] = String::new();
        let p = parse_prompt_v2(&untitled).expect("still a box");
        assert!(p.head_off_screen);
        assert_eq!(p.kind, PromptKind::Question);
        let d = p.question_dialog.expect("the dialog");
        assert!(
            matches!(d.form, QuestionForm::Unknown { .. }),
            "{:?}",
            d.form
        );
        assert_eq!(worker_phase(&untitled), Phase::Prompt);
        // Quoted, the composer under it: no box.
        let mut quoted = r.clone();
        quoted.push(String::new());
        quoted.extend(composer("  ? for shortcuts"));
        assert_eq!(parse_prompt_v2(&quoted), None);
        assert_ne!(worker_phase(&quoted), Phase::Prompt);
    }
}

/// F1 and R8: the preview form's options are read left of the pane (`┌` at
/// column 34), their labels wrapping inside the option column, a trailing
/// ` ✔` the selection tick; the chat row has no number and the form is
/// chosen by arrows and Enter (a digit only moves the focus, measured).
/// NEGATIVE CONTROL: the notes row changed (a person's notes) is unknown.
#[test]
fn the_preview_form_reads_its_options_left_of_the_pane() {
    let (r, p, d) = read(QUESTION_PREVIEW);
    let QuestionForm::Preview {
        options,
        chat,
        pane_col,
        ..
    } = &d.form
    else {
        panic!("{:?}", d.form)
    };
    let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, ["Sidebar (Recommended)", "Tabs", "Single scroll"]);
    assert_eq!(*pane_col, 34);
    assert_eq!(chat.n, None);
    assert!(
        options
            .iter()
            .all(|o| o.description.is_empty() && !o.selected)
    );
    assert_eq!(p.select, Select::ArrowsEnter);
    assert_eq!(worker_phase(&r), Phase::Prompt);
    // The notes open: unknown.
    let mut notes = r.clone();
    let at = row_of(&r, "Notes: press n to add notes");
    notes[at] = format!("{}Notes: keep the sidebar narrow", " ".repeat(34));
    let d = parse_prompt_v2(&notes)
        .and_then(|p| p.question_dialog)
        .expect("still the question");
    assert_eq!(why(&d), NOTES_OPEN);
    // R8: a label wrapped inside the option column.
    let (_, _, d) = read(QUESTION_PREVIEW_LONG_LABEL);
    let o = options_of_preview(&d);
    assert_eq!(
        o[0].label,
        "Keep the settings in a left sidebar (Recommended)"
    );
    assert_eq!(o[0].recommended, Recommended::Label);
    assert_eq!(o[0].label_rows.len(), 2);
    assert_eq!(o[1].label, "Tabs");
    // R8: a revisited tab's tick is the selection, not part of the label.
    let (_, _, d) = read(QUESTION_PREVIEW_REVISITED);
    let o = options_of_preview(&d);
    assert_eq!(o[0].label, "Sidebar (Recommended)");
    assert!(o[0].selected);
    assert_eq!(o[0].recommended, Recommended::Label);
    assert!(!o[1].selected && !o[2].selected);
    assert!(d.submit_tab);
    assert_eq!(d.tabs.len(), 2);
}

fn options_of_preview(d: &QuestionDialog) -> &[QuestionOption] {
    match &d.form {
        QuestionForm::Preview { options, .. } => options,
        other => panic!("not the preview: {other:?}"),
    }
}

/// F1 and R13: multi-select reads each checkbox, the free-text row (which
/// checks itself when typed into) and the button row by position; ONE
/// multi-select question draws the `✔ Submit` chip, and its button reads
/// `Submit`.
#[test]
fn multiselect_reads_checkboxes_the_free_text_and_the_button() {
    let (_, _, d) = read(QUESTION_MULTISELECT);
    let QuestionForm::Multi {
        options,
        free_text,
        button,
        chat,
        ..
    } = &d.form
    else {
        panic!("{:?}", d.form)
    };
    let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Box drawing (Recommended)",
            "Emoji",
            "Powerline (Recommended)"
        ]
    );
    assert_eq!(options[1].description, "A colour emoji fallback font.");
    assert_eq!(
        (free_text.n, free_text.checked, free_text.pristine),
        (4, Some(false), true)
    );
    assert_eq!(free_text.text, "Type something");
    assert_eq!((button.label.as_str(), button.focused), ("Next", false));
    assert_eq!(chat.n, Some(5));
    let (_, _, d) = read(QUESTION_MULTISELECT_TYPED);
    let f = free_row(&d);
    assert_eq!(
        (f.text.as_str(), f.checked, f.pristine),
        ("2", Some(true), false)
    );
    let (_, _, d) = read(QUESTION_MULTISELECT_NEXT);
    assert_eq!(d.focus(), QuestionFocus::Button);
    // R13: a lone multi-select question.
    let (r, p, d) = read(QUESTION_MULTISELECT_ALONE);
    assert!(d.submit_tab);
    assert_eq!(d.tabs.len(), 1);
    assert_eq!(d.tabs[0].header, "Glyphs");
    let QuestionForm::Multi { button, .. } = &d.form else {
        panic!("{:?}", d.form)
    };
    assert_eq!(button.label, "Submit");
    assert_eq!(
        tab_bar(&r[p.span.0]).map(|(t, s)| (t.len(), s)),
        Some((1, true))
    );
}

/// F1: the free-text row is read by its POSITION: a person's `2` in it is
/// no pristine placeholder, whether the focus is on it or moved away; the
/// pristine row with the focus on it reads as focused.
#[test]
fn the_free_text_row_is_read_by_its_position() {
    for text in [QUESTION_FREE_TEXT_TYPED, QUESTION_FREE_TEXT_TYPED_AWAY] {
        let (_, _, d) = read(text);
        let f = free_row(&d);
        assert_eq!((f.n, f.text.as_str(), f.pristine), (4, "2", false));
    }
    let (_, _, d) = read(QUESTION_FREE_TEXT_FOCUSED);
    let f = free_row(&d);
    assert!(f.pristine && f.focused);
    assert_eq!(d.focus(), QuestionFocus::FreeText);
    // A person's text that reads like an option label is still theirs.
    let mut r = screen(QUESTION_FREE_TEXT_TYPED);
    let at = row_of(&r, "❯ 4. 2");
    r[at] = "❯ 4. Dark (Recommended)".to_string();
    let d = parse_prompt_v2(&r)
        .and_then(|p| p.question_dialog)
        .expect("the dialog");
    assert!(!free_row(&d).pristine);
    assert_eq!(options(&d).len(), 3);
}

/// F1: `(Recommended)` is read from the LABEL only: the 80-column capture's
/// wrapped label is [`Recommended::Wrapped`] (its row ran to the edge), the
/// 180-column one [`Recommended::Label`]. NEGATIVE CONTROLS: a description
/// row that ends ` (Recommended)` under a label that fits its row by far is
/// no label (`No`); a label row that cannot be measured (a non-ASCII
/// character) may be wrapped, so a row under it may be its label.
#[test]
fn recommended_is_read_from_the_label_only() {
    let (r, _, d) = read(QUESTION_LONG_LABEL_80COL);
    let o = &options(&d)[0];
    let at = row_of(&r, "meter (Recommended)");
    assert_eq!(o.recommended, Recommended::Wrapped { label_end: at });
    assert_eq!(
        o.label,
        "Keep the outlined accent button on every row that carries a live progress meter (Recommended)"
    );
    assert_eq!(o.label_rows, o.row..at + 1);
    assert!(o.description.starts_with("An accent ring"));
    let (_, _, d) = read(QUESTION_LONG_LABEL_180COL);
    assert_eq!(options(&d)[0].recommended, Recommended::Label);
    // A description row ending ` (Recommended)`: no wrap, no label.
    let mut r = screen(QUESTION_ONE);
    let at = row_of(&r, "A light background with dark text.");
    r[at] = "     A light background with dark text. (Recommended)".to_string();
    let d = parse_prompt_v2(&r)
        .and_then(|p| p.question_dialog)
        .expect("the dialog");
    assert_eq!(options(&d)[1].recommended, Recommended::No);
    assert_eq!(options(&d)[1].label, "Light");
    // Its label not measurable (`é`): the row under it may be its wrap.
    let label = row_of(&r, "2. Light");
    r[label] = "  2. Light é".to_string();
    let d = parse_prompt_v2(&r)
        .and_then(|p| p.question_dialog)
        .expect("the dialog");
    assert_eq!(
        options(&d)[1].recommended,
        Recommended::Wrapped { label_end: at }
    );
    assert!(possibly_wrap("  2. Light é", "     A", 180));
    assert!(!possibly_wrap("  2. Light", "     A", 180));
    assert!(possibly_wrap(&"x".repeat(79), "meter", 80));
}

/// F1: a question whose head the pane cut — its tab bar, its question or
/// its first options off the rows read — is a question with its head off
/// the screen, form unknown, never idle; so is a review tab cut the same
/// way. A cut at the top rule leaves the whole dialog on the rows read, and
/// it reads whole. The dialog drawn at the top of a 49-row grid and read
/// through the window's 40-row tail is cut the same way.
#[test]
fn a_cut_question_is_head_off_screen_never_idle() {
    let r = screen(QUESTION_TABS_INCIDENT);
    let rule = r
        .iter()
        .position(|x| crate::phase::is_rule(x))
        .expect("rule");
    let tail = |from: usize| r[from..].to_vec();
    let whole = parse_prompt_v2(&tail(rule)).expect("from the rule");
    assert!(!whole.head_off_screen);
    assert!(!matches!(
        whole.question_dialog.expect("dialog").form,
        QuestionForm::Unknown { .. }
    ));
    for (what, from) in [
        ("the tab bar", rule + 1),
        ("the question", rule + 3),
        ("option 2", row_of(&r, "2. Solid everywhere")),
        ("the free-text row", row_of(&r, "3. Type something.")),
    ] {
        let t = tail(from);
        let p = parse_prompt_v2(&t).unwrap_or_else(|| panic!("{what}: no box"));
        assert!(p.head_off_screen, "{what}");
        assert_eq!(p.kind, PromptKind::Question, "{what}");
        let d = p.question_dialog.expect("the dialog");
        assert!(matches!(d.form, QuestionForm::Unknown { .. }), "{what}");
        let reading = crate::reader::read(Some("claude"), &t, None);
        assert_eq!(reading.phase, Phase::Prompt, "{what}");
    }
    // The dialog alone at the top of a 49-row pane, read by a 40-row tail.
    let mut grid = tail(rule);
    grid.resize(49, String::new());
    let t40 = grid[9..].to_vec();
    let p = parse_prompt_v2(&t40).expect("the cut dialog");
    assert!(p.head_off_screen);
    assert_eq!(p.kind, PromptKind::Question);
    assert_eq!(worker_phase(&t40), Phase::Prompt);
    assert!(!parse_prompt_v2(&grid).expect("whole").head_off_screen);
    // The review, cut.
    let r = screen(QUESTION_REVIEW);
    let rule = r
        .iter()
        .position(|x| crate::phase::is_rule(x))
        .expect("rule");
    for from in [rule + 1, row_of(&r, "Review your answers"), rule + 6] {
        let t = r[from..].to_vec();
        let p = parse_prompt_v2(&t).unwrap_or_else(|| panic!("review from {from}: no box"));
        assert!(p.head_off_screen, "review from {from}");
        assert_eq!(p.kind, PromptKind::Question);
        assert!(
            matches!(
                p.question_dialog.expect("dialog").form,
                QuestionForm::Unknown { .. }
            ),
            "review from {from}"
        );
        assert_eq!(worker_phase(&t), Phase::Prompt, "review from {from}");
    }
    assert!(!parse_prompt_v2(&r[rule..]).expect("whole").head_off_screen);
}

/// F1: the hand-built column-one dialogs (a shape 2.1.282 does not draw)
/// are still questions — prompt phase — but not read whole, so nothing
/// answers them.
#[test]
fn a_column_one_question_is_still_a_question_not_read_whole() {
    for text in [QUESTION_YES_NO, QUESTION_DO_YOU_WANT] {
        let (r, p, d) = read(text);
        assert_eq!(p.kind, PromptKind::Question);
        assert_eq!(why(&d), NAMED_ONLY);
        assert_eq!(d.guard_row(), None);
        assert_eq!(d.focus(), QuestionFocus::None);
        assert_eq!(worker_phase(&r), Phase::Prompt);
    }
    // The live geometry of the same question reads whole, nothing marked.
    let (_, p, d) = read(QUESTION_DO_YOU_WANT_LIVE);
    assert_eq!(p.question, None, "a question dialog's question is no box's");
    assert_eq!(
        d.question().map(|q| q.text.as_str()),
        Some("Do you want to proceed?")
    );
    let rec: Vec<Recommended> = options(&d).iter().map(|o| o.recommended).collect();
    assert_eq!(rec, [Recommended::No, Recommended::No]);
    assert_eq!(p.with_role(Role::Once), None);
}

/// F1: the owner's TRANSCRIPTION of the incident screen (`›` for the
/// cursor, `□` chips, `✓ Submit`, a leading space on the tab row) is a
/// question — its footer says so — read not whole: the `›` row is no
/// option, so nothing answers it.
#[test]
fn the_transcribed_incident_is_escalated_not_answered() {
    let mut r = screen(QUESTION_TABS_INCIDENT);
    let bar = r.iter().position(|x| x.starts_with('←')).expect("bar");
    r[bar] = " ←  □ Button  □ Grey cursor  □ Glyphs  □ Placement  ✓ Submit  →".to_string();
    let opt = row_of(&r, "❯ 1. Outlined on meters (Recommended)");
    r[opt] = "› 1. Outlined on meters (Recommended)".to_string();
    let p = parse_prompt_v2(&r).expect("a box");
    assert_eq!(p.kind, PromptKind::Question);
    let d = p.question_dialog.expect("the dialog");
    assert!(
        matches!(d.form, QuestionForm::Unknown { .. }),
        "{:?}",
        d.form
    );
    assert_eq!(worker_phase(&r), Phase::Prompt);
    // The `›` alone is enough.
    let mut only = screen(QUESTION_TABS_INCIDENT);
    only[opt] = r[opt].clone();
    let d = parse_prompt_v2(&only)
        .and_then(|p| p.question_dialog)
        .expect("the dialog");
    assert!(matches!(d.form, QuestionForm::Unknown { .. }));
}

/// R5 as MEASURED (the live E2E of 2026-09-25, 2.1.282): with `draft` typed
/// into the composer while the turn ran, the dialog was drawn in place of
/// the composer anyway — the draft hidden under it, no withheld notice —
/// and it reads whole, as any live dialog does; once answered, the draft is
/// back in the composer, untouched. What makes the vendor withhold the
/// dialog instead (the notice below, hand-built) is not measured.
#[test]
fn a_dialog_drawn_over_a_draft_reads_whole_and_the_draft_is_left() {
    let (r, p, d) = read(QUESTION_OVER_DRAFT);
    assert_eq!(p.kind, PromptKind::Question);
    assert!(!p.head_off_screen);
    assert_eq!(worker_phase(&r), Phase::Prompt);
    assert_eq!(d.focus(), QuestionFocus::Option(1));
    let QuestionForm::Single {
        options, question, ..
    } = &d.form
    else {
        panic!("{:?}", d.form)
    };
    assert_eq!(
        question.text,
        "Which color scheme should the demo page use?"
    );
    assert_eq!(options[0].label, "Dark (Recommended)");
    assert_eq!(options[0].recommended, Recommended::Label);
    assert!(
        !crate::phase::has_composer_frame(&r),
        "the draft's composer is not drawn"
    );
    let after = screen(QUESTION_OVER_DRAFT_AFTER);
    assert!(parse_prompt_v2(&after).is_none(), "no box once answered");
    assert_eq!(
        crate::phase::composer_draft(&after).map(|(_, lines)| lines),
        Some(vec!["draft".to_string()])
    );
    assert_ne!(worker_phase(&after), Phase::Prompt);
}

/// R5: Claude Code WITHHOLDS the dialog while the composer holds a draft and
/// draws a dim notice instead — it read idle, a silent stall. It is a
/// question, form unknown, prompt phase. NEGATIVE CONTROLS: the notice's
/// words in the worker's message, the notice over the transcript's later
/// words, and the notice with no composer on the screen, are none.
#[test]
fn a_question_withheld_behind_a_draft_is_a_question_never_idle() {
    let (r, p, d) = read(QUESTION_DEFERRED);
    assert_eq!(p.kind, PromptKind::Question);
    assert!(!p.head_off_screen);
    assert_eq!(why(&d), WITHHELD);
    assert_eq!(worker_phase(&r), Phase::Prompt);
    let notice = row_of(&r, crate::anchors::anchor_text("dialog.deferred_question"));
    assert_eq!(p.span, (notice, notice));
    // The suggestion's notice too, and either wrapped in a narrow pane.
    let mut s = r.clone();
    s[notice] = crate::anchors::anchor_text("dialog.deferred_suggestion").to_string();
    assert_eq!(
        why(&parse_prompt_v2(&s)
            .and_then(|p| p.question_dialog)
            .expect("d")),
        WITHHELD
    );
    let mut wrapped = r.clone();
    wrapped.splice(
        notice..=notice,
        [
            "Claude has a question for you — it shows once you send or clear".to_string(),
            "what you're typing.".to_string(),
        ],
    );
    assert_eq!(worker_phase(&wrapped), Phase::Prompt);
    // Controls.
    let mut said = r.clone();
    said[notice] = format!("⏺ {}", r[notice]);
    assert_ne!(worker_phase(&said), Phase::Prompt);
    let mut history = r.clone();
    history.insert(
        notice + 1,
        "⏺ Never mind, I picked the dark scheme.".to_string(),
    );
    assert_ne!(worker_phase(&history), Phase::Prompt);
    let bare: Vec<String> = r[..=notice].to_vec();
    assert_ne!(worker_phase(&bare), Phase::Prompt);
}

/// R6: what the dialog's host draws under it — the AFK countdown row, one
/// right-aligned plugin notice, the REPL's `… waiting while this panel is
/// open` row — keeps the dialog read whole (they had made it unknown, a
/// stall); the AFK row is read as `auto_continue`. NEGATIVE CONTROL: any
/// other row under the footer, and two plugin-like rows, are not the
/// host's: the dialog is not read whole.
#[test]
fn the_rows_under_a_dialog_keep_it_read_whole() {
    for text in [QUESTION_AFK, QUESTION_PLUGIN_NOTICE, QUESTION_PANEL_WAITING] {
        let (r, p, d) = read(text);
        assert!(
            !matches!(d.form, QuestionForm::Unknown { .. }),
            "{:?}",
            d.form
        );
        assert_eq!(p.span.1, r.len() - 2, "the span ends at the footer");
        assert_eq!(d.focus(), QuestionFocus::Option(1));
    }
    let (_, _, d) = read(QUESTION_AFK);
    assert_eq!(
        d.auto_continue.as_deref(),
        Some("auto-continue in 16s · any key to stay")
    );
    for extra in [
        vec!["Some words a shell printed".to_string()],
        vec![
            format!("{}one note", " ".repeat(160)),
            format!("{}two notes", " ".repeat(160)),
        ],
    ] {
        let mut r = screen(QUESTION_TABS_INCIDENT);
        r.extend(extra.clone());
        let d = parse_prompt_v2(&r).and_then(|p| p.question_dialog);
        assert!(
            d.as_ref()
                .is_none_or(|d| matches!(d.form, QuestionForm::Unknown { .. })),
            "{extra:?}: {d:?}"
        );
    }
}

/// R7: a question footer whose rows are none of the shapes read, over
/// which the old walk finds no box, is still a question when it is LIVE —
/// read with its head off the screen, form unknown, never idle. NEGATIVE
/// CONTROL: the owner's screen QUOTED in a message with the composer under
/// it is no box, and the phase is what it was without the quote.
#[test]
fn a_live_question_footer_is_never_no_box_and_a_quoted_one_is() {
    // The chat row focused and the tab bar unreadable: the old walk stops at
    // the `❯` row and finds nothing.
    let mut r = screen(QUESTION_CHAT_FOCUSED);
    let bar = r.iter().position(|x| x.starts_with('←')).expect("bar");
    r[bar] = "← Button · Grey cursor →".to_string();
    let p = parse_prompt_v2(&r).expect("the question");
    assert!(p.head_off_screen);
    assert_eq!(p.kind, PromptKind::Question);
    let chat = row_of(&r, "❯ 4. Chat about this");
    assert_eq!(p.span, (chat, r.len() - 1));
    assert!(matches!(
        p.question_dialog.expect("dialog").form,
        QuestionForm::Unknown { .. }
    ));
    assert_eq!(worker_phase(&r), Phase::Prompt);
    // Quoted in the user's message, the composer under it.
    let idle = {
        let mut x = crate::prompt::fixtures::rows(&["⏺ Done.", ""]);
        x.extend(composer("  ? for shortcuts"));
        x
    };
    let dialog = screen(QUESTION_CHAT_FOCUSED);
    let rule = dialog
        .iter()
        .position(|x| crate::phase::is_rule(x))
        .expect("rule");
    let mut quoted =
        crate::prompt::fixtures::rows(&["⏺ Done.", "", "❯ The harness showed me this:"]);
    quoted.extend(dialog[rule..].iter().map(|x| format!("  {x}")));
    quoted.push(String::new());
    quoted.extend(composer("  ? for shortcuts"));
    assert_eq!(parse_prompt_v2(&quoted), None);
    assert_eq!(worker_phase(&quoted), worker_phase(&idle));
    // Unindented (a paste that kept column 0), still over the composer.
    let mut flat = crate::prompt::fixtures::rows(&["❯ The harness showed me this:", ""]);
    flat.extend(dialog[rule..].iter().cloned());
    flat.push(String::new());
    flat.extend(composer("  ? for shortcuts"));
    assert_eq!(parse_prompt_v2(&flat), None);
}

/// R9 and R11: a scrolled option list (`↓`/`↑` in the pointer column), a
/// gap in the numbering, a chat row that is not `N+2`, and no row carrying
/// the focus are each unknown — never read whole — and still a question.
#[test]
fn an_unsound_option_list_is_never_read_whole() {
    let (r, p, d) = read(QUESTION_SCROLLED);
    assert_eq!(why(&d), SCROLLED);
    assert_eq!(p.kind, PromptKind::Question);
    assert_eq!(worker_phase(&r), Phase::Prompt);
    // Scrolled up: `↑ 2.` first, the free-text row now in view.
    let mut up = r.clone();
    let one = row_of(&r, "❯ 1. Alpha (Recommended)");
    up.drain(one..one + 2);
    let two = row_of(&up, "2. Beta");
    up[two] = "↑ 2. Beta".to_string();
    let four = row_of(&up, "↓ 4. Delta");
    up[four] = "❯ 4. Delta".to_string();
    up.insert(four + 2, "  5. Type something.".to_string());
    assert_eq!(
        why(&parse_prompt_v2(&up)
            .and_then(|p| p.question_dialog)
            .expect("d")),
        SCROLLED
    );
    // A numbering gap; a chat row not N+2; no focus.
    let base = screen(QUESTION_ONE);
    let edit = |at: &str, to: &str| {
        let mut x = base.clone();
        let i = row_of(&x, at);
        x[i] = to.to_string();
        parse_prompt_v2(&x)
            .and_then(|p| p.question_dialog)
            .expect("the dialog")
    };
    assert_eq!(why(&edit("2. Light", "  3. Light")), NUMBERING);
    assert_eq!(
        why(&edit("5. Chat about this", "  6. Chat about this")),
        CHAT_NUMBER
    );
    assert_eq!(
        why(&edit("❯ 1. Dark (Recommended)", "  1. Dark (Recommended)")),
        FOCUS
    );
    assert_eq!(
        why(&edit("2. Light", "❯ 2. Light")),
        FOCUS,
        "two rows carry the focus"
    );
}

/// The tab bar's grammar: one chip at column 1, or `←` … `✔ Submit` … `→`
/// at column 0 (R13: ONE multi-select question draws the latter).
#[test]
fn the_tab_bar_is_read_by_its_chips() {
    let names = |row: &str| {
        tab_bar(row).map(|(t, s)| {
            (
                t.iter()
                    .map(|c| format!("{}{}", if c.answered { "x " } else { "" }, c.header))
                    .collect::<Vec<_>>(),
                s,
            )
        })
    };
    assert_eq!(
        names(" ☐ Colors"),
        Some((vec!["Colors".to_string()], false))
    );
    assert_eq!(
        names("←  ☒ Button  ☐ Grey cursor  ✔ Submit  →"),
        Some((
            vec!["x Button".to_string(), "Grey cursor".to_string()],
            true
        ))
    );
    assert_eq!(
        names("←  ☐ Glyphs  ✔ Submit  →"),
        Some((vec!["Glyphs".to_string()], true))
    );
    for not in [
        "☐ Colors",
        "  ☐ Colors",
        " ☐ A  ☐ B",
        "←  ☐ Button  ☐ Grey cursor  →",
        "←  ✔ Submit  →",
        " ←  □ Button  ✓ Submit  →",
        "⏺ ☐ Write the tests",
    ] {
        assert_eq!(tab_bar(not), None, "{not}");
    }
}

/// The accessors a decider reads: the guard row, the focus, one tab
/// against the next.
#[test]
fn the_dialogs_accessors_name_its_rows() {
    let (_, _, first) = read(QUESTION_TABS_INCIDENT);
    let (_, _, second) = read(QUESTION_TABS_SECOND);
    let (_, _, chat) = read(QUESTION_CHAT_FOCUSED);
    let (_, _, review) = read(QUESTION_REVIEW);
    let (_, _, unanswered) = read(QUESTION_REVIEW_UNANSWERED);
    assert!(
        first.same_question(&chat),
        "the focus moved, the tab did not"
    );
    assert!(!first.same_question(&second));
    assert!(review.same_question(&unanswered));
    assert!(!review.same_question(&first));
    let unknown = QuestionDialog::unknown(NAMED_ONLY);
    assert!(!unknown.same_question(&unknown));
    assert_eq!(unknown.question(), None);
    assert_eq!(unknown.guard_row(), None);
    assert_eq!(unknown.focus(), QuestionFocus::None);
}

/// The dialog as 2.1.281 drew it (live captures of 2026-09-24, before the
/// 2.1.282 captures this module was built on) reads whole too: the tab bar
/// (`☐ Tea` alone for one question), each answer's description row off its
/// label, the `(Recommended)` mark, the multi-select checkboxes and button,
/// and the review tab — never `Unknown`, never idle. NEGATIVE CONTROLS: the
/// same dialogs over the composer frame (a transcript quoting them) are no
/// box; under the worker's words the review is no box, and a footed dialog
/// is a question not read whole — escalated, never answered.
#[test]
fn the_2_1_281_captures_read_whole() {
    let (r, p, d) = read(ASK_TWO_FIRST);
    assert_eq!(worker_phase(&r), Phase::Prompt);
    assert_eq!(p.title, "←  ☐ Color  ☐ Size  ✔ Submit  →");
    assert_eq!(
        d.question().map(|q| q.text.as_str()),
        Some("Which color would you prefer?")
    );
    let labels: Vec<(&str, Recommended, bool)> = options(&d)
        .iter()
        .map(|o| (o.label.as_str(), o.recommended, o.focused))
        .collect();
    assert_eq!(
        labels,
        [
            ("Red", Recommended::No, true),
            ("Blue (Recommended)", Recommended::Label, false),
            ("Green", Recommended::No, false),
        ]
    );
    assert!(p.options.iter().all(|o| o.role == Role::Other));
    assert_eq!(p.select, Select::Digits);

    let (_, p, d) = read(ASK_TWO_SECOND);
    assert_eq!(p.title, "←  ☒ Color  ☐ Size  ✔ Submit  →");
    assert_eq!(options(&d)[0].label, "Small");
    assert!(options(&d).iter().all(|o| o.recommended == Recommended::No));

    let (_, p, d) = read(ASK_ONE);
    assert_eq!((p.kind, p.title.as_str()), (PromptKind::Question, "☐ Tea"));
    assert!(matches!(d.form, QuestionForm::Single { .. }));
    assert_eq!(options(&d).len(), 2);

    let (_, _, d) = read(ASK_MULTI);
    let QuestionForm::Multi { button, .. } = &d.form else {
        panic!("multi-select: {:?}", d.form);
    };
    assert_eq!(button.label, "Submit");
    assert!(options(&d).iter().all(|o| o.checked == Some(false)));
    let (_, p, d) = read(ASK_MULTI_CHECKED);
    assert_eq!(p.title, "←  ☒ Toppings  ✔ Submit  →");
    assert_eq!(options(&d)[0].checked, Some(true));

    let (r, _, d) = read(ASK_SUBMIT);
    assert_eq!(worker_phase(&r), Phase::Prompt);
    assert!(matches!(
        d.form,
        QuestionForm::Review {
            unanswered: false,
            ..
        }
    ));
    assert_eq!(d.focus(), QuestionFocus::ReviewSubmit);

    for text in [ASK_TWO_FIRST, ASK_ONE, ASK_MULTI, ASK_SUBMIT] {
        let mut quoted = screen(text);
        quoted.extend(composer("  ? for shortcuts"));
        assert_eq!(
            parse_prompt_v2(&quoted).map(|p| p.kind),
            None,
            "a composer under it"
        );
        let mut said = screen(text);
        said.push("⏺ Done.".to_string());
        match parse_prompt_v2(&said) {
            None => assert_eq!(text, ASK_SUBMIT, "the review under words is no box"),
            Some(p) => {
                assert_eq!(p.kind, PromptKind::Question);
                why(&p.question_dialog.expect("its dialog"));
            }
        }
    }
}
