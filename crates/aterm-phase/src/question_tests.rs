// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the question dialog reader against live captures and negative controls.

use std::collections::BTreeSet;

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

/// The incident screen (S6-01, a four-tab dialog at 180 columns) reads whole.
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

/// Every live 2.1.282 capture reads whole as its form (focus, `(Recommended)`,
/// checkboxes, free-text row, tabs): prompt phase, authoritative, every role `Other`.
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

/// A bare question at column 0 (S6-02) is inside the box, not a shell line ending it;
/// the chat row's `❯` at column 0 is the dialog's focus, not a user's row.
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

/// The Submit (review) tab, which draws no footer, is the question dialog.
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
        assert_eq!(p.footer, None);
        let mut moved = r.clone();
        moved[submit.row] = "  1. Submit answers".to_string();
        moved[cancel.row] = "❯ 2. Cancel".to_string();
        let d = parse_prompt_v2(&moved)
            .and_then(|p| p.question_dialog)
            .expect("the review");
        assert_eq!(d.focus(), QuestionFocus::ReviewCancel);
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
        let mut quoted = r.clone();
        quoted.push(String::new());
        quoted.extend(composer("  ? for shortcuts"));
        assert_eq!(parse_prompt_v2(&quoted), None);
        assert_ne!(worker_phase(&quoted), Phase::Prompt);
    }
}

/// THE REVIEW TAB WITH AN AUTO-DENY COUNTDOWN — HAND-BUILT where 2.1.301's
/// code draws it (its permission reason block, between the answers and
/// `Ready to submit your answers?`; not measured): the countdown row, and
/// the same wrapped onto a second row as a narrow pane draws it, and through
/// its very words onto three in a pane narrower than them, each over its own
/// blank row, is passed over — the review is read whole, its four answers
/// and its focus as before — and the box's countdown reads its time.
/// NEGATIVE CONTROLS: any other row at column 0 there (another `⚠` warning,
/// one or wrapped, a countdown with no blank row under it) is no review: its
/// head off the screen, form unknown, escalated.
#[test]
fn the_review_tab_is_read_whole_over_its_countdown() {
    let r = screen(QUESTION_REVIEW);
    let ready = row_of(&r, "Ready to submit your answers?");
    let with = |block: &[&str], blank_under: bool| {
        let mut t = r.clone();
        let mut add: Vec<String> = block.iter().map(|s| (*s).to_string()).collect();
        if blank_under {
            add.push(String::new());
        }
        t.splice(ready..ready, add);
        t
    };
    let one = "⚠ Claude Code will automatically deny this request in 0:14, to avoid blocking \
               progress on an unattended session";
    for block in [
        vec![one],
        vec![
            "⚠ Claude Code will automatically deny this request in 0:14, to avoid",
            "blocking progress on an unattended session",
        ],
        // A pane narrower than its words: `deny
        // this request in` wrapped too.
        vec![
            "⚠ Claude Code will automatically deny this",
            "request in 0:14, to avoid blocking progress on",
            "an unattended session",
        ],
    ] {
        let t = with(&block, true);
        let p = parse_prompt_v2(&t).expect("the review");
        let d = p.question_dialog.clone().expect("the dialog");
        assert!(
            matches!(
                d.form,
                QuestionForm::Review {
                    unanswered: false,
                    ..
                }
            ),
            "{block:?}: {:?}",
            d.form
        );
        assert_eq!(d.tabs.len(), 4, "{block:?}");
        assert_eq!(d.focus(), QuestionFocus::ReviewSubmit, "{block:?}");
        assert!(!p.head_off_screen, "{block:?}");
        assert_eq!(
            p.auto_deny_left(),
            Some(std::time::Duration::from_secs(14)),
            "{block:?}: {p:?}"
        );
    }
    for (why, t) in [
        (
            "another warning",
            with(&["⚠ Something else the vendor warns about"], true),
        ),
        (
            "another warning, wrapped",
            with(
                &["⚠ Something else the vendor warns", "about, at some length"],
                true,
            ),
        ),
        ("no blank row under it", with(&[one], false)),
    ] {
        let p = parse_prompt_v2(&t).expect("still a box");
        assert!(p.head_off_screen, "{why}");
        let d = p.question_dialog.expect("the dialog");
        assert!(
            matches!(d.form, QuestionForm::Unknown { .. }),
            "{why}: {:?}",
            d.form
        );
    }
}

/// The preview form's options are read left of the pane (`┌` at column 34), labels
/// wrapping inside the option column, a trailing ` ✔` the selection tick; the chat row
/// has no number, and a digit only moves the focus.
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
    let mut notes = r.clone();
    let at = row_of(&r, "Notes: press n to add notes");
    notes[at] = format!("{}Notes: keep the sidebar narrow", " ".repeat(34));
    let d = parse_prompt_v2(&notes)
        .and_then(|p| p.question_dialog)
        .expect("still the question");
    assert_eq!(why(&d), NOTES_OPEN);
    let (_, _, d) = read(QUESTION_PREVIEW_LONG_LABEL);
    let o = options_of_preview(&d);
    assert_eq!(
        o[0].label,
        "Keep the settings in a left sidebar (Recommended)"
    );
    assert_eq!(o[0].recommended, Recommended::Label);
    assert_eq!(o[0].label_rows.len(), 2);
    assert_eq!(o[1].label, "Tabs");
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

/// Multi-select reads each checkbox, the free-text row and the button row by position;
/// one multi-select question draws the `✔ Submit` chip, and its button reads `Submit`.
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

/// The free-text row is read by its position: a person's `2` in it is no pristine
/// placeholder, focused or not; the pristine row with the focus on it reads as focused.
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
    let mut r = screen(QUESTION_FREE_TEXT_TYPED);
    let at = row_of(&r, "❯ 4. 2");
    r[at] = "❯ 4. Dark (Recommended)".to_string();
    let d = parse_prompt_v2(&r)
        .and_then(|p| p.question_dialog)
        .expect("the dialog");
    assert!(!free_row(&d).pristine);
    assert_eq!(options(&d).len(), 3);
}

/// `(Recommended)` is read from the label only: the 80-column capture's wrapped label is
/// [`Recommended::Wrapped`], the 180-column one [`Recommended::Label`].
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
    let mut r = screen(QUESTION_ONE);
    let at = row_of(&r, "A light background with dark text.");
    r[at] = "     A light background with dark text. (Recommended)".to_string();
    let d = parse_prompt_v2(&r)
        .and_then(|p| p.question_dialog)
        .expect("the dialog");
    assert_eq!(options(&d)[1].recommended, Recommended::No);
    assert_eq!(options(&d)[1].label, "Light");
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

/// A question whose tab bar, question or first options the pane cut (or a review tab
/// cut the same way, or a dialog at the top of a 49-row grid read by a 40-row tail) has
/// its head off the screen, form unknown, never idle.
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
    let mut grid = tail(rule);
    grid.resize(49, String::new());
    let t40 = grid[9..].to_vec();
    let p = parse_prompt_v2(&t40).expect("the cut dialog");
    assert!(p.head_off_screen);
    assert_eq!(p.kind, PromptKind::Question);
    assert_eq!(worker_phase(&t40), Phase::Prompt);
    assert!(!parse_prompt_v2(&grid).expect("whole").head_off_screen);
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

/// F2, measured on Claude Code 2.1.302: a multi-select
/// whose policy text wraps over more rows than a read's top leaves of the
/// dialog.
#[test]
fn a_multi_select_cut_inside_its_free_text_row_is_head_off_screen() {
    let grid = screen(QUESTION_MULTI_TYPED_41X60_2_1_302);
    assert_eq!(grid.len(), 53, "PRECONDITION: the grid from its row 7");
    let (_, whole, d) = read(QUESTION_MULTI_TYPED_41X60_2_1_302);
    assert!(!whole.head_off_screen);
    let free = free_row(&d);
    assert!(free.focused && free.checked == Some(true) && !free.pristine);
    assert!(free.text.starts_with("Decide yourself: choose the option"));
    assert!(free.text.ends_with("The user was not asked.)"));
    let cut_at = |rows: &[String], from: usize, what: &str| {
        let t = rows[from..].to_vec();
        let p = parse_prompt_v2(&t).unwrap_or_else(|| panic!("{what}: no box"));
        assert!(p.head_off_screen, "{what}: {:?}", p.question_dialog);
        assert_eq!(p.kind, PromptKind::Question, "{what}");
        assert_eq!(
            why(&p.question_dialog.expect("the dialog")),
            HEAD_CUT,
            "{what}"
        );
        assert_eq!(worker_phase(&t), Phase::Prompt, "{what}");
    };
    let tail = grid.len() - 40;
    assert!(
        grid[tail].starts_with("         ") && tail > free.row,
        "PRECONDITION: the tail begins inside the text: {:?}",
        grid[tail]
    );
    cut_at(&grid, tail, "the 40-row tail");
    let button = row_of(&grid, "Submit");
    for from in free.row + 1..=button + 1 {
        cut_at(&grid, from, &format!("typed, from row {from}"));
    }
    let (cols, kept) = QUESTION_MULTI_REFLOW_LONG_KEPT_2_1_302[1];
    assert_eq!(cols, 41);
    let (r, _, d) = read(kept);
    let free = free_row(&d);
    assert!(!free.focused && free.checked == Some(true));
    let button = row_of(&r, "❯    Submit");
    assert!(
        r[free.row + 1].starts_with("        o"),
        "PRECONDITION: column 8"
    );
    for from in free.row + 1..=button + 1 {
        cut_at(&r, from, &format!("after down, from row {from}"));
    }
}

/// A multi-select cut at ANY row from its first option down to its divider
/// is the dialog with its head off the screen, wherever the focus is: on an
/// option (the pristine dialog at 37, 41 and 80 columns), in the typed text
/// (41x60), or on `Submit` after `down` (the nine widths of
/// `QUESTION_MULTI_REFLOW_LONG_KEPT_2_1_302`), all measured on Claude Code
/// 2.1.302, and the 2.1.283 captures at 120 columns.
#[test]
fn a_multi_select_cut_at_any_row_under_its_head_is_head_off_screen() {
    let mut frames: Vec<(String, &str)> = vec![
        (
            "typed 41x60".to_string(),
            QUESTION_MULTI_TYPED_41X60_2_1_302,
        ),
        (
            "2.1.283 typed".to_string(),
            QUESTION_283_MULTISELECT_TYPED_LONG_TERM,
        ),
        (
            "2.1.283 Submit".to_string(),
            QUESTION_283_MULTISELECT_TYPED_BUTTON,
        ),
        (
            "2.1.283 pristine".to_string(),
            QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED,
        ),
    ];
    for (cols, capture) in QUESTION_MULTI_PRISTINE_2_1_302 {
        frames.push((format!("pristine at {cols}"), capture));
    }
    for (cols, capture) in QUESTION_MULTI_REFLOW_LONG_KEPT_2_1_302 {
        frames.push((format!("Submit at {cols}"), capture));
    }
    let mut buttons = 0;
    let mut misread: Vec<String> = Vec::new();
    for (what, capture) in frames {
        let (r, whole, d) = read(capture);
        assert!(!whole.head_off_screen, "{what}: PRECONDITION: read whole");
        assert!(matches!(d.form, QuestionForm::Multi { .. }), "{what}");
        let divider = row_of(&r, "5. Chat about this") - 1;
        if button_row(&r[divider - 1]).is_some_and(|(_, focused)| focused) {
            buttons += 1;
        }
        misread.extend(
            cuts_under_the_head(&r, &what)
                .into_iter()
                .filter_map(|(at, off, form)| {
                    (!off || !matches!(form, QuestionForm::Unknown { why } if why == HEAD_CUT))
                        .then(|| format!("{at}: {form:?}"))
                }),
        );
    }
    assert_eq!(
        buttons, 10,
        "PRECONDITION: the focus on `Submit` in ten frames"
    );
    assert!(
        misread.is_empty(),
        "read with the head on the screen: {misread:#?}"
    );
}

/// Each cut of a multi-select's whole dialog `r` from its option 1 down to
/// its divider, read: where it was cut (for `what`), whether its head is off
/// the screen, and its form.
fn cuts_under_the_head(r: &[String], what: &str) -> Vec<(String, bool, QuestionForm)> {
    let first = r
        .iter()
        .position(|x| x.trim_start_matches('❯').trim_start().starts_with("1. ["))
        .expect("option 1");
    let divider = row_of(r, "5. Chat about this") - 1;
    assert!(is_rule(&r[divider]), "{what}: PRECONDITION: the divider");
    (first..=divider)
        .map(|from| {
            let t = r[from..].to_vec();
            let at = format!("{what}, cut at {:?}", r[from]);
            let p = parse_prompt_v2(&t).unwrap_or_else(|| panic!("{at}: no box"));
            assert_eq!(p.kind, PromptKind::Question, "{at}");
            assert_eq!(worker_phase(&t), Phase::Prompt, "{at}");
            let form = p.question_dialog.expect("the dialog").form;
            (at, p.head_off_screen, form)
        })
        .collect()
}

/// , on Claude Code 2.1.302: every frame of
/// the four reflow sweeps from 33 columns (under it option 1 wraps), both
/// policy texts typed and `down` to `Submit`, rebuilt whole
/// ([`reflow_dialog`], first held to the nine measured dialogs it redraws
/// row for row), is the dialog with its head off the screen when cut at
/// any row from option 1 down to the divider, wherever the whole reads as
/// the multi-select — every frame the text kept its rows in (the widths
/// the harness types at, column 8 and column 9) and the frames `Submit`
/// was drawn over that still read.
#[test]
fn a_multi_select_cut_under_its_head_is_head_off_screen_at_every_swept_width() {
    let template = QUESTION_MULTI_REFLOW_LONG_KEPT_2_1_302[1].1;
    let long = reflow_frames(QUESTION_MULTI_REFLOW_LONG_SWEEP_2_1_302);
    for (cols, capture) in QUESTION_MULTI_REFLOW_LONG_KEPT_2_1_302 {
        let (_, frame) = long.iter().find(|(c, _)| *c == cols).expect("swept");
        assert_eq!(
            reflow_dialog(template, frame, cols),
            screen(capture),
            "PRECONDITION: the dialog measured at {cols} columns redrawn"
        );
    }
    let mut misread: Vec<String> = Vec::new();
    for (name, sweeps, swept, kept_in) in [
        (
            "long-term",
            [
                QUESTION_MULTI_REFLOW_LONG_SWEEP_2_1_302,
                QUESTION_MULTI_REFLOW_LONG_WIDE_2_1_302,
            ],
            159,
            137,
        ),
        (
            "short-term",
            [
                QUESTION_MULTI_REFLOW_SHORT_SWEEP_2_1_302,
                QUESTION_MULTI_REFLOW_SHORT_WIDE_2_1_302,
            ],
            149,
            130,
        ),
    ] {
        let (mut frames, mut kept) = (0, 0);
        let mut cols_at = BTreeSet::new();
        for (cols, frame) in sweeps.into_iter().flat_map(reflow_frames) {
            if cols < 33 {
                continue;
            }
            frames += 1;
            let r = reflow_dialog(template, &frame, cols);
            let what = format!("{name} at {cols} columns");
            let whole = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{what}: no box"));
            assert!(!whole.head_off_screen, "{what}: PRECONDITION: whole");
            let reads = matches!(
                whole.question_dialog.expect("the dialog").form,
                QuestionForm::Multi { .. }
            );
            if frame[frame.len() - 2] == "❯    Submit" {
                kept += 1;
                cols_at.insert(if frame[0].starts_with("  4. [✔] ") {
                    9
                } else {
                    8
                });
                if !reads {
                    misread.push(format!("{what}: kept, not read whole"));
                }
            }
            misread.extend(cuts_under_the_head(&r, &what).into_iter().filter_map(
                |(at, off, form)| {
                    let cut = if reads {
                        off && matches!(form, QuestionForm::Unknown { why } if why == HEAD_CUT)
                    } else {
                        matches!(form, QuestionForm::Unknown { .. })
                    };
                    (!cut).then(|| format!("{at}: {form:?}"))
                },
            ));
        }
        assert_eq!(
            (frames, kept),
            (swept, kept_in),
            "{name}: PRECONDITION: the frames from 33 columns, and those kept"
        );
        assert_eq!(cols_at, BTreeSet::from([8, 9]), "{name}: both columns");
    }
    assert!(
        misread.is_empty(),
        "read with the head on the screen: {misread:#?}"
    );
}

/// The hand-built column-one dialogs (a shape 2.1.282 does not draw) are questions but
/// not read whole, so nothing answers them.
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

/// A hand transcription of the incident screen (`›` for the cursor, `□` chips, `✓
/// Submit`, a leading space on the tab row) is a question by its footer, not read whole:
/// the `›` row is no option, so nothing answers it.
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
    let mut only = screen(QUESTION_TABS_INCIDENT);
    only[opt] = r[opt].clone();
    let d = parse_prompt_v2(&only)
        .and_then(|p| p.question_dialog)
        .expect("the dialog");
    assert!(matches!(d.form, QuestionForm::Unknown { .. }));
}

/// Measured on 2.1.282: a draft typed while the turn ran does not withhold the dialog;
/// it is drawn over the composer, the draft hidden under it, and reads whole.
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

/// The dim notice Claude Code draws in place of a dialog it withholds behind a draft is a
/// question, form unknown, prompt phase.
#[test]
fn a_question_withheld_behind_a_draft_is_a_question_never_idle() {
    let (r, p, d) = read(QUESTION_DEFERRED);
    assert_eq!(p.kind, PromptKind::Question);
    assert!(!p.head_off_screen);
    assert_eq!(why(&d), WITHHELD);
    assert_eq!(worker_phase(&r), Phase::Prompt);
    let notice = row_of(&r, crate::anchors::anchor_text("dialog.deferred_question"));
    assert_eq!(p.span, (notice, notice));
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

/// What the dialog's host draws under it (the AFK countdown, one right-aligned plugin
/// notice, the panel-waiting row) keeps the dialog read whole; the AFK row is read as
/// `auto_continue`.
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

/// A live question footer over rows of no shape read is still a question: head off the
/// screen, form unknown, never idle.
#[test]
fn a_live_question_footer_is_never_no_box_and_a_quoted_one_is() {
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
    let mut flat = crate::prompt::fixtures::rows(&["❯ The harness showed me this:", ""]);
    flat.extend(dialog[rule..].iter().cloned());
    flat.push(String::new());
    flat.extend(composer("  ? for shortcuts"));
    assert_eq!(parse_prompt_v2(&flat), None);
}

/// A scrolled option list (`↓`/`↑` in the pointer column), a numbering gap, a chat row
/// that is not `N+2`, and no focused row are each unknown, and still a question.
#[test]
fn an_unsound_option_list_is_never_read_whole() {
    let (r, p, d) = read(QUESTION_SCROLLED);
    assert_eq!(why(&d), SCROLLED);
    assert_eq!(p.kind, PromptKind::Question);
    assert_eq!(worker_phase(&r), Phase::Prompt);
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

/// The tab bar's grammar: one chip at column 1, or `←` … `✔ Submit` … `→` at column 0
/// (which one multi-select question draws too).
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

/// Copies of `aterm-agent`'s `LONG_TERM_TEXT` and `SHORT_TERM_TEXT` (no commas,
/// semicolons, hyphens or `then`, so a multi-select answer is not delivered CSV-quoted),
/// pinning the reader's side: each, wrapped as 2.1.283 wraps the field, reads back whole.
const REWORDED_LONG_TERM: &str = "Decide yourself: choose the option that best serves long term correctness and total value even at the cost of short term deliverables. Say which you chose and why in one line and continue without asking again. (Answered by the aterm harness by policy. The user was not asked.)";
const REWORDED_SHORT_TERM: &str = "Decide yourself: choose the option that gets the current deliverable done soonest at acceptable risk. Say which you chose and why in one line and continue without asking again. (Answered by the aterm harness by policy. The user was not asked.)";

/// The captures whose free-text row holds a measured policy text, with it: the focused
/// field, and the field the focus left for the button (c3-06, c4-10).
const TYPED_2_1_283: [(&str, &str, &str); 7] = [
    (
        "c1-04",
        QUESTION_283_TYPED_LONG_TERM,
        MEASURED_LONG_TERM_2_1_283,
    ),
    (
        "c4-04",
        QUESTION_283_TABS_FIRST_TYPED_LONG_TERM,
        MEASURED_LONG_TERM_2_1_283,
    ),
    (
        "c8-02",
        QUESTION_283_TYPED_SHORT_TERM,
        MEASURED_SHORT_TERM_2_1_283,
    ),
    (
        "c3-05",
        QUESTION_283_MULTISELECT_TYPED_LONG_TERM,
        MEASURED_LONG_TERM_2_1_283,
    ),
    (
        "c4-09",
        QUESTION_283_TABS_SECOND_TYPED_LONG_TERM,
        MEASURED_LONG_TERM_2_1_283,
    ),
    (
        "c3-06",
        QUESTION_283_MULTISELECT_TYPED_BUTTON,
        MEASURED_LONG_TERM_2_1_283,
    ),
    (
        "c4-10",
        QUESTION_283_TABS_SECOND_TYPED_BUTTON,
        MEASURED_LONG_TERM_2_1_283,
    ),
];

/// The rows the free-text row's text is drawn on: from its numbered row to
/// the multi-select button or the divider.
fn field_rows(d: &QuestionDialog) -> std::ops::Range<usize> {
    match &d.form {
        QuestionForm::Single {
            free_text, chat, ..
        } => free_text.row..chat.row - 1,
        QuestionForm::Multi {
            free_text, button, ..
        } => free_text.row..button.row,
        other => panic!("no free-text row: {other:?}"),
    }
}

/// One 2.1.283 capture's expected reading.
struct Live283 {
    name: &'static str,
    text: &'static str,
    /// `single`, `multi`, `preview`, `review`, or the `why` of an unknown.
    form: &'static str,
    focus: QuestionFocus,
    answered: &'static [bool],
    /// The free-text row's (text, pristine, checked), where the form has one.
    free: Option<(&'static str, bool, Option<bool>)>,
}

/// Every 2.1.283 capture of the dialog (c1…c11) reads as its form, or unknown where it
/// must: a question, its head on the screen, prompt phase, authoritative.
#[test]
fn every_2_1_283_question_capture_reads_as_its_form() {
    use QuestionFocus as F;
    const Y: bool = true;
    const N: bool = false;
    const DOT: &str = "Type something.";
    const BARE: &str = "Type something";
    const LONG: &str = MEASURED_LONG_TERM_2_1_283;
    fn live(
        name: &'static str,
        text: &'static str,
        form: &'static str,
        focus: QuestionFocus,
        answered: &'static [bool],
        free: Option<(&'static str, bool, Option<bool>)>,
    ) -> Live283 {
        Live283 {
            name,
            text,
            form,
            focus,
            answered,
            free,
        }
    }
    let table = [
        live(
            "c1-03",
            QUESTION_283_FREE_TEXT_FOCUSED,
            "single",
            F::FreeText,
            &[N],
            Some((DOT, Y, None)),
        ),
        live(
            "c1-04",
            QUESTION_283_TYPED_LONG_TERM,
            "single",
            F::FreeText,
            &[N],
            Some((LONG, N, None)),
        ),
        live(
            "c8-02",
            QUESTION_283_TYPED_SHORT_TERM,
            "single",
            F::FreeText,
            &[N],
            Some((MEASURED_SHORT_TERM_2_1_283, N, None)),
        ),
        live(
            "c2-04",
            QUESTION_283_TYPED_PERSON,
            "single",
            F::FreeText,
            &[N],
            Some(("go with blue", N, None)),
        ),
        live(
            "c3-04",
            QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED,
            "multi",
            F::FreeText,
            &[N],
            Some((BARE, Y, Some(N))),
        ),
        live(
            "c3-05",
            QUESTION_283_MULTISELECT_TYPED_LONG_TERM,
            "multi",
            F::FreeText,
            &[Y],
            Some((LONG, N, Some(Y))),
        ),
        live(
            "c3-06",
            QUESTION_283_MULTISELECT_TYPED_BUTTON,
            "multi",
            F::Button,
            &[Y],
            Some((LONG, N, Some(Y))),
        ),
        live(
            "c3-07",
            QUESTION_283_MULTISELECT_REVIEW,
            "review",
            F::ReviewSubmit,
            &[Y],
            None,
        ),
        live(
            "c3b-03",
            QUESTION_283_MULTISELECT_TYPED_PERSON,
            "multi",
            F::FreeText,
            &[Y],
            Some(("go with olives", N, Some(Y))),
        ),
        live(
            "c3c-03",
            QUESTION_283_MULTISELECT_UNTICKED,
            "multi",
            F::FreeText,
            &[N],
            Some(("go with olives", N, Some(N))),
        ),
        live(
            "c4-01",
            QUESTION_283_TABS_FIRST,
            "single",
            F::Option(1),
            &[N, N],
            Some((DOT, Y, None)),
        ),
        live(
            "c4-02",
            QUESTION_283_TABS_FIRST_DOWN,
            "single",
            F::Option(2),
            &[N, N],
            Some((DOT, Y, None)),
        ),
        live(
            "c4-03",
            QUESTION_283_TABS_FIRST_FREE_TEXT_FOCUSED,
            "single",
            F::FreeText,
            &[N, N],
            Some((DOT, Y, None)),
        ),
        live(
            "c4-04",
            QUESTION_283_TABS_FIRST_TYPED_LONG_TERM,
            "single",
            F::FreeText,
            &[N, N],
            Some((LONG, N, None)),
        ),
        live(
            "c4-05",
            QUESTION_283_TABS_SECOND,
            "multi",
            F::Option(1),
            &[Y, N],
            Some((BARE, Y, Some(N))),
        ),
        live(
            "c4-06",
            QUESTION_283_TABS_SECOND_DOWN,
            "multi",
            F::Option(2),
            &[Y, N],
            Some((BARE, Y, Some(N))),
        ),
        live(
            "c4-07",
            QUESTION_283_TABS_SECOND_DOWN_TWICE,
            "multi",
            F::Option(3),
            &[Y, N],
            Some((BARE, Y, Some(N))),
        ),
        live(
            "c4-08",
            QUESTION_283_TABS_SECOND_FREE_TEXT_FOCUSED,
            "multi",
            F::FreeText,
            &[Y, N],
            Some((BARE, Y, Some(N))),
        ),
        live(
            "c4-09",
            QUESTION_283_TABS_SECOND_TYPED_LONG_TERM,
            "multi",
            F::FreeText,
            &[Y, Y],
            Some((LONG, N, Some(Y))),
        ),
        live(
            "c4-10",
            QUESTION_283_TABS_SECOND_TYPED_BUTTON,
            "multi",
            F::Button,
            &[Y, Y],
            Some((LONG, N, Some(Y))),
        ),
        live(
            "c4-11",
            QUESTION_283_TABS_REVIEW,
            "review",
            F::ReviewSubmit,
            &[Y, Y],
            None,
        ),
        live(
            "c11-01",
            QUESTION_283_PREVIEW,
            "preview",
            F::Option(1),
            &[N],
            None,
        ),
        live(
            "c5-03",
            QUESTION_283_PREVIEW_CHAT_FOCUSED,
            FOCUS,
            F::None,
            &[N],
            None,
        ),
        live(
            "c5-06",
            QUESTION_283_PREVIEW_NOTES_OPEN,
            NOTES_OPEN,
            F::None,
            &[N],
            None,
        ),
    ];
    for want in &table {
        let name = want.name;
        let (r, p, d) = read(want.text);
        assert_eq!(p.kind, PromptKind::Question, "{name}");
        assert!(!p.head_off_screen, "{name}");
        assert_eq!(d.width, 120, "{name}");
        let form = match &d.form {
            QuestionForm::Single { .. } => "single",
            QuestionForm::Multi { .. } => "multi",
            QuestionForm::Preview { .. } => "preview",
            QuestionForm::Review { .. } => "review",
            QuestionForm::Unknown { why } => why,
        };
        assert_eq!(form, want.form, "{name}");
        assert_eq!(d.focus(), want.focus, "{name}");
        let answered: Vec<bool> = d.tabs.iter().map(|t| t.answered).collect();
        assert_eq!(answered, want.answered, "{name}");
        assert_eq!(
            d.submit_tab,
            want.answered.len() > 1 || form == "multi" || form == "review",
            "{name}"
        );
        let free = match &d.form {
            QuestionForm::Single { free_text, .. } | QuestionForm::Multi { free_text, .. } => {
                Some((
                    free_text.text.as_str(),
                    free_text.pristine,
                    free_text.checked,
                ))
            }
            _ => None,
        };
        assert_eq!(free, want.free, "{name}");
        match &d.form {
            QuestionForm::Single { options, .. } => {
                let o: Vec<(&str, Recommended)> = options
                    .iter()
                    .map(|o| (o.label.as_str(), o.recommended))
                    .collect();
                assert_eq!(
                    o,
                    [
                        ("Red (Recommended)", Recommended::Label),
                        ("Blue", Recommended::No)
                    ],
                    "{name}"
                );
            }
            QuestionForm::Multi { options, .. } => {
                let o: Vec<(&str, Option<bool>)> = options
                    .iter()
                    .map(|o| (o.label.as_str(), o.checked))
                    .collect();
                assert_eq!(
                    o,
                    [
                        ("Cheese (Recommended)", Some(N)),
                        ("Olives", Some(N)),
                        ("Basil", Some(N))
                    ],
                    "{name}: no option is ever ticked"
                );
            }
            QuestionForm::Review {
                unanswered,
                submit,
                cancel,
            } => {
                assert!(!unanswered, "{name}");
                assert_eq!(r[submit.row].trim(), "❯ 1. Submit answers", "{name}");
                assert_eq!(cancel.row, submit.row + 1, "{name}");
                assert_eq!(d.guard_row(), Some(submit.row), "{name}");
                assert_eq!(p.span.1, cancel.row, "{name}");
            }
            _ => {}
        }
        let reading = crate::reader::read(Some("claude"), &r, None);
        assert_eq!(reading.phase, Phase::Prompt, "{name}");
        assert!(reading.phase_authoritative, "{name}");
    }
}

/// 2.1.283: the policy text typed into the free-text row (three rows at the label
/// column) reads back as the rows joined with one space, exactly the text typed: the
/// read-back the harness compares before any Enter.
#[test]
fn the_typed_policy_text_reads_back_exactly_as_typed() {
    for (name, text, measured) in TYPED_2_1_283 {
        let (r, _, d) = read(text);
        assert_eq!(free_row(&d).text, measured, "{name}");
        let rows = field_rows(&d);
        assert_eq!(rows.len(), 3, "{name}: the field grew to three rows");
        let col = if matches!(d.form, QuestionForm::Multi { .. }) {
            9
        } else {
            5
        };
        for (i, row) in r[rows].iter().enumerate().skip(1) {
            assert_eq!(leading_spaces(row), col, "{name}: the field's row {i}");
        }
    }
    for (typed, button) in [
        (
            QUESTION_283_MULTISELECT_TYPED_LONG_TERM,
            QUESTION_283_MULTISELECT_TYPED_BUTTON,
        ),
        (
            QUESTION_283_TABS_SECOND_TYPED_LONG_TERM,
            QUESTION_283_TABS_SECOND_TYPED_BUTTON,
        ),
    ] {
        let (a, _, da) = read(typed);
        let (b, _, db) = read(button);
        let (ra, rb) = (field_rows(&da), field_rows(&db));
        assert_ne!(a[ra.start + 1], b[rb.start + 1], "the field reflowed");
        assert_eq!(free_row(&da).text, free_row(&db).text);
        assert!(da.same_question(&db));
        assert_eq!(
            (da.focus(), db.focus()),
            (QuestionFocus::FreeText, QuestionFocus::Button)
        );
    }
    for text in [
        QUESTION_283_TYPED_PERSON,
        QUESTION_283_MULTISELECT_TYPED_PERSON,
    ] {
        let (_, _, d) = read(text);
        let f = free_row(&d);
        assert!(!f.pristine);
        assert_ne!(f.text, MEASURED_LONG_TERM_2_1_283);
        assert_eq!(field_rows(&d).len(), 1);
    }
}

/// 2.1.283: the preview form reads whole on its first screen (c11-01); two `down`s past
/// its last option draw `❯` on the chat row and leave it on option 2 (c5-03), and `n`
/// opens the notes (c5-06): each unknown, still a question, never answered.
#[test]
fn the_2_1_283_preview_is_unknown_on_its_chat_row_and_with_its_notes_open() {
    let (_, _, d) = read(QUESTION_283_PREVIEW);
    let QuestionForm::Preview {
        options, pane_col, ..
    } = &d.form
    else {
        panic!("{:?}", d.form)
    };
    assert_eq!(*pane_col, 34);
    assert_eq!(options[0].label, "Stacked (Recommended)");
    for (text, want, drawn) in [
        (
            QUESTION_283_PREVIEW_CHAT_FOCUSED,
            FOCUS,
            "❯ Chat about this",
        ),
        (
            QUESTION_283_PREVIEW_NOTES_OPEN,
            NOTES_OPEN,
            "Notes: Add notes on this design…",
        ),
    ] {
        let (r, p, d) = read(text);
        assert!(r.iter().any(|x| x.trim() == drawn), "{want}");
        assert_eq!(why(&d), want);
        assert_eq!(p.kind, PromptKind::Question);
        assert!(!p.head_off_screen);
        assert_eq!(worker_phase(&r), Phase::Prompt);
    }
    let (r, p, _) = read(QUESTION_283_PREVIEW_CHAT_FOCUSED);
    let focused: Vec<&str> = r[p.span.0..=p.span.1]
        .iter()
        .filter(|x| x.starts_with('❯'))
        .map(|x| x.trim_end())
        .collect();
    assert_eq!(focused.len(), 2);
    assert!(focused[0].starts_with("❯ 2. Side by side  "));
    assert_eq!(focused[1], "❯ Chat about this");
}

/// The policy texts, typed into each 2.1.283 select tab (single and multi-select, alone
/// and one of two, focused and left for the button) and wrapped as 2.1.283 wraps them,
/// read back whole and exactly at every width from the narrowest the tab's other rows
/// fit in to 220.
#[test]
fn the_reworded_policy_texts_read_whole_at_80_and_200_columns() {
    let check = |capture: &str, text: &str, width: usize| {
        let (_, _, measured) = read(capture);
        let r = rewrap_free_text(capture, text, width);
        let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{width}: no box"));
        assert_eq!(p.kind, PromptKind::Question, "{width}");
        assert!(!p.head_off_screen, "{width}");
        let d = p.question_dialog.expect("the dialog");
        assert_eq!(d.width, width);
        assert_eq!(
            std::mem::discriminant(&d.form),
            std::mem::discriminant(&measured.form),
            "{width}: {:?}",
            d.form
        );
        let f = free_row(&d);
        assert_eq!(f.text, text, "{width}");
        assert!(!f.pristine, "{width}");
        assert_eq!(f.checked, free_row(&measured).checked, "{width}");
        assert_eq!(d.focus(), measured.focus(), "{width}");
        assert!(d.same_question(&measured), "{width}");
        assert_eq!(worker_phase(&r), Phase::Prompt, "{width}");
        field_rows(&d).len()
    };
    for (capture, narrowest) in [
        (QUESTION_283_TYPED_LONG_TERM, 80),
        (QUESTION_283_TYPED_SHORT_TERM, 80),
        (QUESTION_283_MULTISELECT_TYPED_LONG_TERM, 80),
        (QUESTION_283_MULTISELECT_TYPED_BUTTON, 80),
        (QUESTION_283_TABS_FIRST_TYPED_LONG_TERM, 84),
        (QUESTION_283_TABS_SECOND_TYPED_LONG_TERM, 84),
        (QUESTION_283_TABS_SECOND_TYPED_BUTTON, 84),
    ] {
        let (r, p, d) = read(capture);
        let field = field_rows(&d);
        let widest = (p.span.0..r.len())
            .filter(|i| !field.contains(i) && !is_rule(&r[*i]))
            .map(|i| r[i].chars().count())
            .max()
            .expect("rows");
        assert_eq!(widest.max(80), narrowest);
        for text in [REWORDED_LONG_TERM, REWORDED_SHORT_TERM] {
            let at_narrowest = check(capture, text, narrowest);
            let at_200 = check(capture, text, 200);
            assert!(
                at_narrowest >= 4 && at_200 == 2,
                "{at_narrowest} rows at {narrowest}, {at_200} at 200: wrapped at both"
            );
            for width in narrowest..=220 {
                check(capture, text, width);
            }
        }
    }
}

/// A free-text row whose wrapped rows are not at its label column (one cell either side,
/// or a multi-select row's at column 5) is a question never read whole: a vendor that
/// moves the wrap stalls the harness rather than have it read the text short.
#[test]
fn a_free_text_row_wrapped_at_another_column_is_never_read_whole() {
    for (capture, cols) in [
        (QUESTION_283_TYPED_LONG_TERM, &[4, 6][..]),
        (QUESTION_283_TYPED_SHORT_TERM, &[4, 6]),
        (QUESTION_283_MULTISELECT_TYPED_LONG_TERM, &[5, 8, 10]),
        (QUESTION_283_MULTISELECT_TYPED_BUTTON, &[5, 8, 10]),
    ] {
        for text in [REWORDED_LONG_TERM, REWORDED_SHORT_TERM] {
            let r = rewrap_free_text(capture, text, 80);
            let d = parse_prompt_v2(&r)
                .and_then(|p| p.question_dialog)
                .expect("read whole where it is drawn");
            let rows = field_rows(&d);
            assert_eq!(free_row(&d).text, text);
            for k in rows.start + 1..rows.end {
                for &col in cols {
                    let mut moved = r.clone();
                    moved[k] = format!("{}{}", " ".repeat(col), r[k].trim_start());
                    let p = parse_prompt_v2(&moved)
                        .unwrap_or_else(|| panic!("row {k} at {col}: no box"));
                    assert_eq!(p.kind, PromptKind::Question, "row {k} at {col}");
                    let d = p.question_dialog.expect("the dialog");
                    assert!(
                        matches!(d.form, QuestionForm::Unknown { .. }),
                        "row {k} at {col}: {:?}",
                        d.form
                    );
                    assert_eq!(worker_phase(&moved), Phase::Prompt, "row {k} at {col}");
                }
            }
        }
    }
}

/// The 2.1.281 captures read whole too: the tab bar (`☐ Tea` alone for one question),
/// each description off its label, `(Recommended)`, the multi-select checkboxes and
/// button, and the review tab.
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

/// In a pane narrower than the footer (48 columns or fewer for one question, 59 for tabs)
/// Ink wraps `Enter to select · … · Esc to cancel` onto the row under it; each measured
/// 2.1.295 screen reads whole, the recommended option focused, the box running to the
/// footer's last row.
#[test]
fn a_question_whose_footer_wraps_is_read_whole() {
    let last = |r: &[String]| r.iter().rposition(|x| !x.trim().is_empty()).expect("a row");
    const ONE: &str = "Enter to select · ↑/↓ to navigate · Esc to cancel";
    const TABS: &str = "Enter to select · Tab/Arrow keys to navigate · Esc to cancel";
    for (text, recommended, footer) in [
        (QUESTION_ONE_44_2_1_295, "nextest (Recommended)", ONE),
        (QUESTION_ONE_36_2_1_295, "nextest (Recommended)", ONE),
        (QUESTION_TABS_56_2_1_295, "Disk (Recommended)", TABS),
    ] {
        let (r, p, d) = read(text);
        assert_eq!(worker_phase(&r), Phase::Prompt, "{recommended}");
        assert!(!p.head_off_screen);
        let o = &options(&d)[0];
        assert_eq!(
            (o.label.as_str(), o.recommended, o.focused),
            (recommended, Recommended::Label, true)
        );
        assert_eq!(p.span.1, last(&r), "the box runs to the footer's last row");
        assert_eq!(p.footer.as_deref(), Some(footer), "the footer, joined");
    }
    let r = screen(QUESTION_ONE_44_2_1_295);
    let end = last(&r);
    let mut short = r.clone();
    short[end - 1] = "Enter to select · Esc to".to_string();
    let mut moved = r.clone();
    moved[end] = format!("  {}", r[end]);
    let mut quoted = r.clone();
    let rule = "─".repeat(44);
    quoted.extend([
        rule.clone(),
        "❯".to_string(),
        rule,
        "  ? for shortcuts".to_string(),
    ]);
    for rows in [short, moved, quoted] {
        assert_eq!(parse_prompt_v2(&rows), None, "{rows:#?}");
    }
}

/// Claude Code 2.1.300 MEASURED (2026-10-15, the
/// real-Claude lab): a multi-select whose free-text row holds the long-term
/// text, typed by hand, after `down` to `Submit` — at 47 columns every row
/// kept, the text drawn one cell left of its label column (`[✔]Decide`); at
/// 39 the same, `Submit` drawn over the text's last row.
const REFLOW_39_2_1_300: &str =
    include_str!("fixtures/claude-2.1.300-question-multi-reflow-39col.txt");
const REFLOW_47_2_1_300: &str =
    include_str!("fixtures/claude-2.1.300-question-multi-reflow-47col.txt");
/// At 57 columns `Submit` is drawn over the text's last row and the residue
/// of that row is glued to it, one word: `❯    Submitked.)`.
const REFLOW_57_2_1_300: &str =
    include_str!("fixtures/claude-2.1.300-question-multi-reflow-57col.txt");

/// `text` in rows `width` cells wide as 2.1.300 draws a multi-select's
/// unfocused free-text row (Ink's `wrap-ansi` with `trim: false`, MEASURED
/// with the long-term text at 43 widths from 38 to 150 columns,
/// `aterm-agent`'s `question::drawn_rows`): greedy at
/// single spaces, save that a row filled to its last cell leaves the next
/// one starting with the space between, which takes a cell and is not
/// drawn.
fn drawn_wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut line = 0usize;
    for (k, word) in text.split(' ').enumerate() {
        if k > 0 {
            if line >= width {
                rows.push(String::new());
                line = 0;
            }
            rows.last_mut().expect("a row").push(' ');
            line += 1;
        }
        if line + word.len() > width {
            rows.push(String::new());
            line = 0;
        }
        rows.last_mut().expect("a row").push_str(word);
        line += word.len();
    }
    rows.iter().map(|r| r.trim().to_string()).collect()
}

/// `text` word-wrapped greedily into rows `width` cells wide (Ink's footer).
fn word_wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows: Vec<String> = vec![String::new()];
    for word in text.split(' ') {
        let line = rows.last_mut().expect("a row");
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            rows.push(word.to_string());
        }
    }
    rows
}

/// A 2.1.300 multi-select `cols` wide, the focus on `Submit`, with `text` in
/// its free-text row drawn from column `col` — 9, its label column, behind
/// `[✔] `, or 8, behind `[✔]` with no space — on the rows [`drawn_wrap`]
/// gives at `cols - col`, and `Submit` on its own row under them.
fn reflowed(text: &str, cols: usize, col: usize) -> Vec<String> {
    let rows = screen(REFLOW_47_2_1_300);
    let free = rows
        .iter()
        .position(|r| r.starts_with("  4. [✔]"))
        .expect("the free-text row");
    let divider = (free..rows.len())
        .find(|&i| is_rule(&rows[i]))
        .expect("the divider");
    let footer = rows
        .iter()
        .position(|r| r.starts_with("Enter to select"))
        .expect("the footer");
    let lead = if col == 9 {
        "  4. [✔] "
    } else {
        "  4. [✔]"
    };
    let mut out: Vec<String> = rows[..free]
        .iter()
        .map(|r| {
            if is_rule(r) {
                "─".repeat(cols)
            } else {
                r.clone()
            }
        })
        .collect();
    for (k, line) in drawn_wrap(text, cols - col).into_iter().enumerate() {
        let at = if k == 0 {
            lead.to_string()
        } else {
            " ".repeat(col)
        };
        out.push(format!("{at}{line}"));
    }
    out.push("❯    Submit".to_string());
    out.push("─".repeat(cols));
    out.extend(rows[divider + 1..footer].iter().cloned());
    let joined = crate::wrap::rejoined(&rows[footer..]);
    out.extend(word_wrap(&joined, cols));
    out
}

/// THE CHECKBOX DRAWN WITHOUT ITS SPACE (F2).
#[test]
fn a_free_text_drawn_against_its_checkbox_reads_whole() {
    let (r, p, d) = read(REFLOW_47_2_1_300);
    assert_eq!(p.kind, PromptKind::Question);
    assert!(!p.head_off_screen);
    assert_eq!(worker_phase(&r), Phase::Prompt);
    let QuestionForm::Multi {
        options,
        free_text,
        button,
        ..
    } = &d.form
    else {
        panic!("not read whole: {:?}", d.form)
    };
    let read_options: Vec<(&str, &str, Option<bool>)> = options
        .iter()
        .map(|o| (o.label.as_str(), o.description.as_str(), o.checked))
        .collect();
    assert_eq!(
        read_options,
        [
            ("Unit tests (Recommended)", "Fast", Some(false)),
            ("Lint", "Style", Some(false)),
            ("Fuzz", "Slow", Some(false)),
        ]
    );
    assert_eq!(free_text.text, REWORDED_LONG_TERM);
    assert_eq!((free_text.pristine, free_text.checked), (false, Some(true)));
    assert_eq!(button.label, "Submit");
    assert_eq!(d.focus(), QuestionFocus::Button);

    assert_eq!(reflowed(REWORDED_LONG_TERM, 47, 8), r);

    for text in [REWORDED_LONG_TERM, REWORDED_SHORT_TERM] {
        for col in [8, 9] {
            for cols in 38..=150 {
                let rows = reflowed(text, cols, col);
                let p = parse_prompt_v2(&rows).unwrap_or_else(|| panic!("{cols}/{col}: no box"));
                assert_eq!(p.kind, PromptKind::Question, "{cols}/{col}");
                let d = p.question_dialog.expect("the dialog");
                let f = free_row(&d);
                assert_eq!(f.text, text, "{cols}/{col}");
                assert_eq!(f.checked, Some(true), "{cols}/{col}");
                assert_eq!(d.focus(), QuestionFocus::Button, "{cols}/{col}");
                assert_eq!(worker_phase(&rows), Phase::Prompt, "{cols}/{col}");

                let first = rows
                    .iter()
                    .position(|x| x.starts_with("  4. [✔]"))
                    .expect("the row");
                let last = rows
                    .iter()
                    .position(|x| x.starts_with("❯    Submit"))
                    .expect("the button");
                for k in first + 1..last {
                    for moved_to in [col - 1, col + 1] {
                        let mut moved = rows.clone();
                        moved[k] = format!("{}{}", " ".repeat(moved_to), rows[k].trim_start());
                        let p = parse_prompt_v2(&moved).unwrap_or_else(|| {
                            panic!("{cols}/{col}: row {k} at {moved_to}: no box")
                        });
                        assert_eq!(p.kind, PromptKind::Question, "{cols}/{col} row {k}");
                        let d = p.question_dialog.expect("the dialog");
                        assert!(
                            matches!(d.form, QuestionForm::Unknown { .. }),
                            "{cols}/{col}: row {k} at {moved_to}: {:?}",
                            d.form
                        );
                        assert_eq!(worker_phase(&moved), Phase::Prompt, "{cols}/{col} row {k}");
                    }
                }
            }
        }
    }

    let (r, p, d) = read(REFLOW_39_2_1_300);
    assert_eq!(p.kind, PromptKind::Question);
    assert!(
        matches!(d.form, QuestionForm::Unknown { .. }),
        "{:?}",
        d.form
    );
    assert_eq!(worker_phase(&r), Phase::Prompt);
}

/// 2.1.300 at 50 columns draws a ticked multi-select free-text row as `4.
#[test]
fn a_ticked_free_text_row_drawn_without_a_space_is_read_whole() {
    let (_, p, d) = read(QUESTION_MULTI_TYPED_50_2_1_300);
    assert_eq!(p.kind, PromptKind::Question);
    let QuestionForm::Multi {
        options, free_text, ..
    } = &d.form
    else {
        panic!("not read whole: {:?}", d.form);
    };
    let labels: Vec<(&str, Option<bool>)> = options
        .iter()
        .map(|o| (o.label.as_str(), o.checked))
        .collect();
    assert_eq!(
        labels,
        [
            ("Unit tests (Recommended)", Some(false)),
            ("Lint", Some(false)),
            ("Fuzz", Some(false))
        ]
    );
    assert_eq!(free_text.checked, Some(true));
    assert!(!free_text.pristine);
    assert_eq!(
        free_text.text,
        "Decide yourself: choose the option that best serves long term correctness and \
         total value even at the cost of short term deliverables. Say which you chose and \
         why in one line and continue without asking again. (Answered by the aterm harness \
         by policy. The user was not asked.)"
    );
    assert_eq!(d.focus(), QuestionFocus::Button);
}

/// 's F2 residual: only a person's keys put the focus on a
/// dialog's `Chat about this` row (the loop never moves it there), and a
/// tail cut AT that row is the dialog with its head off the screen, focused
/// or not: the walk up from the footer finds no divider over the chat row,
/// so it is the fallback ([`QShape::fallback`]) or the footer alone, both
/// with the head off.
#[test]
fn a_cut_at_the_chat_row_a_person_focused_is_the_dialog_with_its_head_off() {
    let mut frames: Vec<(String, &str)> = vec![
        (
            "typed 41x60".to_string(),
            QUESTION_MULTI_TYPED_41X60_2_1_302,
        ),
        (
            "2.1.283 typed".to_string(),
            QUESTION_283_MULTISELECT_TYPED_LONG_TERM,
        ),
        (
            "2.1.283 Submit".to_string(),
            QUESTION_283_MULTISELECT_TYPED_BUTTON,
        ),
        (
            "2.1.283 pristine".to_string(),
            QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED,
        ),
        ("2.1.283 single".to_string(), QUESTION_283_FREE_TEXT_FOCUSED),
    ];
    for (cols, capture) in QUESTION_MULTI_PRISTINE_2_1_302 {
        frames.push((format!("pristine at {cols}"), capture));
    }
    for (cols, capture) in QUESTION_MULTI_REFLOW_LONG_KEPT_2_1_302 {
        frames.push((format!("Submit at {cols}"), capture));
    }
    let mut misread: Vec<String> = Vec::new();
    for (what, capture) in frames {
        let r = screen(capture);
        let chat = r
            .iter()
            .position(|x| {
                x.trim_start_matches('❯')
                    .trim_end()
                    .ends_with(". Chat about this")
            })
            .unwrap_or_else(|| panic!("{what}: the chat row"));
        let focused: Vec<String> = r
            .iter()
            .enumerate()
            .map(|(i, x)| {
                let x = x
                    .strip_prefix('❯')
                    .map_or_else(|| x.clone(), |rest| format!(" {rest}"));
                if i == chat {
                    format!("❯{}", &x[1..])
                } else {
                    x
                }
            })
            .collect();
        let whole = parse_prompt_v2(&focused).unwrap_or_else(|| panic!("{what}: no box"));
        let d = whole.question_dialog.expect("the dialog");
        assert!(
            !whole.head_off_screen
                && d.focus() == QuestionFocus::Chat
                && matches!(
                    d.form,
                    QuestionForm::Single { .. } | QuestionForm::Multi { .. }
                ),
            "{what}: PRECONDITION: read whole, the focus on the chat row: {:?}",
            d.form
        );
        for (grid, focus) in [(&r, "elsewhere"), (&focused, "on it")] {
            let t = grid[chat..].to_vec();
            let at = format!("{what}, cut at {:?}, the focus {focus}", grid[chat]);
            let p = parse_prompt_v2(&t).unwrap_or_else(|| panic!("{at}: no box"));
            assert_eq!(p.kind, PromptKind::Question, "{at}");
            assert_eq!(worker_phase(&t), Phase::Prompt, "{at}");
            let form = p.question_dialog.expect("the dialog").form;
            if !p.head_off_screen || !matches!(form, QuestionForm::Unknown { .. }) {
                misread.push(format!(
                    "{at}: head_off_screen={} {form:?}",
                    p.head_off_screen
                ));
            }
        }
    }
    assert!(
        misread.is_empty(),
        "read with the head on the screen: {misread:#?}"
    );
}

/// (Questions): `Submit` drawn over the text's last row
/// leaves the rest of that row on the button row.
#[test]
fn a_button_row_a_residue_is_glued_to_is_never_read_whole() {
    let (r, p, d) = read(REFLOW_57_2_1_300);
    assert!(
        r.iter().any(|x| x == "❯    Submitked.)"),
        "PRECONDITION: the residue"
    );
    assert_eq!(p.kind, PromptKind::Question);
    assert!(
        matches!(d.form, QuestionForm::Unknown { .. }),
        "57 columns: {:?}",
        d.form
    );
    assert_eq!(worker_phase(&r), Phase::Prompt);
    let template = QUESTION_MULTI_REFLOW_LONG_KEPT_2_1_302[1].1;
    let (mut glued, mut clean) = (0, 0);
    let mut misread: Vec<String> = Vec::new();
    for sweep in [
        QUESTION_MULTI_REFLOW_LONG_SWEEP_2_1_302,
        QUESTION_MULTI_REFLOW_LONG_WIDE_2_1_302,
        QUESTION_MULTI_REFLOW_SHORT_SWEEP_2_1_302,
        QUESTION_MULTI_REFLOW_SHORT_WIDE_2_1_302,
    ] {
        for (cols, frame) in reflow_frames(sweep) {
            if cols < 33 {
                continue;
            }
            let button = &frame[frame.len() - 2];
            let Some(rest) = button.strip_prefix("❯    Submit") else {
                continue;
            };
            let r = reflow_dialog(template, &frame, cols);
            let p = parse_prompt_v2(&r).unwrap_or_else(|| panic!("{cols}: no box"));
            assert_eq!(p.kind, PromptKind::Question, "{cols}");
            assert_eq!(worker_phase(&r), Phase::Prompt, "{cols}");
            let form = p.question_dialog.expect("the dialog").form;
            let whole = !matches!(form, QuestionForm::Unknown { .. });
            if rest.is_empty() {
                clean += 1;
                if !whole {
                    misread.push(format!("{cols}: `Submit` alone, not read whole"));
                }
            } else if !rest.contains(' ') {
                glued += 1;
                if whole {
                    misread.push(format!("{cols}: {button:?} read whole"));
                }
            }
        }
    }
    assert!(
        glued > 0 && clean > 0,
        "PRECONDITION: both kinds of frame ({glued} glued, {clean} clean)"
    );
    assert!(misread.is_empty(), "{misread:#?}");
}
