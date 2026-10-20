// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The policy-text decider (`long-term` and `short-term`) over aterm-phase's
//! captures of Claude Code 2.1.283's question dialog with a text typed into
//! its free-text row, redrawn with this module's own texts
//! (`fixtures::rewrap_free_text`): the texts' rules, the fit estimate, every
//! row of `choose_text`'s table, the recommended path unchanged byte for byte,
//! and the Tier-1 bind of the decider to `SupervisorQuestionText` (`Text = 1`),
//! over tabs the text goes to and tabs that fall back, with its three negative
//! controls.

use std::collections::{BTreeSet, VecDeque};

use aterm_phase::prompt::fixtures::{self as f, rewrap_free_text, screen};
use aterm_phase::{QuestionFocus, QuestionForm, parse_prompt_v2};

use super::super::approval::{
    Answer, AnswerTarget, ApprovalCtx, Choice, Decision, QuestionPriority, RULE_ALLOW_ONCE,
    RULE_ANSWER_LONG_TERM, RULE_PROSE_LONG_TERM, decide,
};
use super::super::guard::row_guard;
use super::tests::{row_of, with_focus};
use super::*;

const LONG: QuestionPriority = QuestionPriority::LongTerm;
const SHORT: QuestionPriority = QuestionPriority::ShortTerm;
/// The width every 2.1.283 capture was taken at.
const COLS: u16 = 120;

/// The text a priority writes (a text priority's).
fn text_of(p: QuestionPriority) -> &'static str {
    p.text().expect("a text priority")
}

/// The decision on `rows` under `p`, the loop remembering `own`.
fn decided(rows: &[String], p: QuestionPriority, own: Option<QuestionPriority>) -> Decision {
    let prompt = parse_prompt_v2(rows).unwrap_or_else(|| panic!("no box: {rows:#?}"));
    answer_question(&prompt, rows, p, own, false, Pane::of(rows))
}

/// What an answer does: writes a priority's text, or presses Enter on a
/// target — after `steps` moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Did {
    Type(i32, QuestionPriority),
    Enter(i32, AnswerTarget),
}

/// The answer a decision chose, with its rule, subject and unproven.
fn did(d: &Decision) -> (Did, &'static str, String, String) {
    match d {
        Decision::Approve {
            rule_id,
            choice: Choice::Answer(a),
            subject,
            unproven,
            ..
        } => {
            let what = match (a.typed(), a.enter_target()) {
                (Some(p), None) => Did::Type(a.steps(), p),
                (None, Some(t)) => Did::Enter(a.steps(), t),
                other => panic!("an answer that both types and enters: {other:?}"),
            };
            (
                what,
                rule_id,
                subject.clone(),
                unproven.clone().expect("an answer says how it chose"),
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

/// `rows` with the first row holding `from` edited to hold `to`.
fn edited(rows: &[String], from: &str, to: &str) -> Vec<String> {
    let at = rows
        .iter()
        .position(|r| r.contains(from))
        .unwrap_or_else(|| panic!("no row holds {from:?}"));
    let mut out = rows.to_vec();
    out[at] = out[at].replacen(from, to, 1);
    out
}

/// The dialog on `rows`.
fn dialog_of(rows: &[String]) -> aterm_phase::QuestionDialog {
    parse_prompt_v2(rows)
        .and_then(|p| p.question_dialog)
        .unwrap_or_else(|| panic!("no dialog: {rows:#?}"))
}

/// The free-text row as read.
fn free_text_of(rows: &[String]) -> aterm_phase::FreeText {
    match dialog_of(rows).form {
        QuestionForm::Single { free_text, .. } | QuestionForm::Multi { free_text, .. } => free_text,
        other => panic!("no free-text row: {other:?}"),
    }
}

// ---- the texts ----------------------------------------------------------

/// A greedy wrap of `text` at single spaces into rows `room` cells wide.
fn wrap(text: &str, room: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split(' ') {
        let line = lines.last_mut().expect("a line");
        if line.is_empty() {
            line.push_str(word);
        } else if line.len() + 1 + word.len() <= room {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(word.to_string());
        }
    }
    lines
}

/// The texts' rules: ASCII only; no digit and none of `, ; " - \` (a digit
/// could select an option, a comma draws CSV quotes around a multi-select
/// answer, `send` reads `\n`); single spaces, none at an edge; no word over 16
/// cells; at most 279 bytes (below the paste-placeholder size); no `key ` (a
/// log reader could take it for a verb). Every wrap of them, greedy or as the
/// dialog draws it, joins back with one space to the text exactly. Negative
/// control: the measured wording breaks the rules (commas and hyphens).
#[test]
fn the_policy_texts_keep_their_rules() {
    let broken = |t: &str| -> Vec<&'static str> {
        let mut why = Vec::new();
        if !t.is_ascii() {
            why.push("not ASCII");
        }
        if t.bytes()
            .any(|b| b.is_ascii_digit() || b",;\"-\\".contains(&b))
        {
            why.push("a digit or one of , ; \" - \\");
        }
        if t.contains("  ") || t.starts_with(' ') || t.ends_with(' ') {
            why.push("a double or edge space");
        }
        if t.split(' ').any(|w| w.len() > 16) {
            why.push("a word over 16 cells");
        }
        if t.len() > 279 {
            why.push("over 279 bytes");
        }
        if t.contains("key ") {
            why.push("`key `");
        }
        why
    };
    assert!(!broken(f::MEASURED_LONG_TERM_2_1_283).is_empty());
    for p in [LONG, SHORT] {
        let text = text_of(p);
        assert_eq!(broken(text), Vec::<&str>::new(), "{p:?}");
        for width in 40..=220usize {
            for label_col in [5usize, 9] {
                for room in [width - label_col, width - 1 - label_col] {
                    let rows = wrap(text, room);
                    assert!(rows.iter().all(|r| r.len() <= room), "{width}/{label_col}");
                    assert_eq!(rows.join(" "), text, "{p:?} at {width}, column {label_col}");
                }
            }
        }
        for capture in [
            f::QUESTION_283_TYPED_LONG_TERM,
            f::QUESTION_283_MULTISELECT_TYPED_LONG_TERM,
            f::QUESTION_283_MULTISELECT_TYPED_BUTTON,
        ] {
            for width in [80, 120, 200] {
                let rows = rewrap_free_text(capture, text, width);
                let field = free_text_of(&rows);
                assert!(!field.pristine, "{p:?} at {width}");
                assert_eq!(field.text, text, "{p:?} read back at {width}: {rows:#?}");
            }
        }
    }
    // The words parse back to their priorities and name question rules.
    for p in QuestionPriority::ALL {
        assert_eq!(QuestionPriority::parse(p.word()), Some(p));
        assert!(is_question_rule(p.rule_id()), "{p:?}");
    }
    assert_eq!(QuestionPriority::Recommended.text(), None);
    // Negative control: no other rule is a question dialog's; a question in
    // prose is the turn end's (`answer@v1`, `prose-…@v1`).
    for rule in [
        RULE_ALLOW_ONCE,
        RULE_PROSE_LONG_TERM,
        super::super::turn_end::RULE_ANSWER,
        "",
        "answer-recommended",
    ] {
        assert!(!is_question_rule(rule), "{rule}");
    }
    for bad in ["long_term", "ask", "LONGTERM", "short term", ""] {
        assert_eq!(QuestionPriority::parse(bad), None, "{bad:?}");
    }
}

/// The fit estimate is never below what Claude Code draws (each measured
/// capture's text took three rows at the label column). The reworded texts fit
/// the measured 120x44 panes, pristine, but not a pane no taller than the
/// dialog, nor one narrower than a word.
#[test]
fn the_fit_estimate_is_at_or_above_the_measured_wraps() {
    let measured_rows = |rows: &[String]| -> usize {
        let field = free_text_of(rows);
        let col = label_col(&field);
        1 + rows[field.row + 1..]
            .iter()
            .take_while(|r| {
                r.chars().take(col).filter(|c| *c == ' ').count() == col
                    && r.chars().nth(col).is_some_and(|c| c != ' ')
            })
            .count()
    };
    for (capture, text) in [
        (
            f::QUESTION_283_TYPED_LONG_TERM,
            f::MEASURED_LONG_TERM_2_1_283,
        ),
        (
            f::QUESTION_283_MULTISELECT_TYPED_LONG_TERM,
            f::MEASURED_LONG_TERM_2_1_283,
        ),
        (
            f::QUESTION_283_TABS_FIRST_TYPED_LONG_TERM,
            f::MEASURED_LONG_TERM_2_1_283,
        ),
        (
            f::QUESTION_283_TABS_SECOND_TYPED_LONG_TERM,
            f::MEASURED_LONG_TERM_2_1_283,
        ),
        (
            f::QUESTION_283_TYPED_SHORT_TERM,
            f::MEASURED_SHORT_TERM_2_1_283,
        ),
    ] {
        let rows = screen(capture);
        let field = free_text_of(&rows);
        assert_eq!(field.text, text, "PRECONDITION: the capture's text");
        let drawn = measured_rows(&rows);
        assert_eq!(drawn, 3, "{capture:.80}");
        let col = label_col(&field);
        let estimated = wrapped_rows(text, usize::from(COLS) - col - 4).expect("wraps");
        assert!(estimated >= drawn, "{estimated} < {drawn}: {capture:.80}");
    }
    // Every pane width from 80 to 220: the estimate is at or above the rows
    // the real wrap (`rewrap_free_text`) draws for this module's texts,
    // single- and multi-select. Negative control: with no margin some width is
    // estimated below what is drawn (the focused field wraps a cell short).
    let mut under = 0;
    for width in 80..=220usize {
        for capture in [
            f::QUESTION_283_TYPED_LONG_TERM,
            f::QUESTION_283_MULTISELECT_TYPED_LONG_TERM,
        ] {
            for text in [LONG_TERM_TEXT, SHORT_TERM_TEXT] {
                let rows = rewrap_free_text(capture, text, width);
                let drawn = measured_rows(&rows);
                let col = label_col(&free_text_of(&rows));
                let estimated = wrapped_rows(text, width - col - WRAP_MARGIN).expect("wraps");
                assert!(estimated >= drawn, "{width}: {estimated} < {drawn}");
                if wrapped_rows(text, width - col).expect("wraps") < drawn {
                    under += 1;
                }
            }
        }
    }
    assert!(under > 0, "no width tells the margin from none");

    for capture in [
        f::QUESTION_283_FREE_TEXT_FOCUSED,
        f::QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED,
        f::QUESTION_283_TABS_FIRST_FREE_TEXT_FOCUSED,
        f::QUESTION_283_TABS_SECOND_FREE_TEXT_FOCUSED,
    ] {
        let rows = screen(capture);
        let prompt = parse_prompt_v2(&rows).expect("a box");
        let col = label_col(&free_text_of(&rows));
        for text in [LONG_TERM_TEXT, SHORT_TERM_TEXT] {
            let pane = |cols: u16| Pane {
                cols,
                rows: rows.len(),
            };
            assert!(
                text_fits(&prompt, &rows, pane(COLS), col, text),
                "{capture:.80}"
            );
            assert!(
                !text_fits(&prompt, &rows, pane(16), col, text),
                "a word wider than the row"
            );
        }
        // The dialog alone, in a pane no taller than itself.
        let top = rows
            .iter()
            .position(|r| !r.is_empty() && r.chars().all(|c| c == '─'))
            .expect("the top rule");
        let last = rows
            .iter()
            .rposition(|r| !r.trim().is_empty())
            .expect("a footer");
        let tight: Vec<String> = rows[top..=last].to_vec();
        let prompt = parse_prompt_v2(&tight).expect("a box");
        assert!(
            !text_fits(&prompt, &tight, Pane::of(&tight), col, LONG_TERM_TEXT),
            "{capture:.80}"
        );
        // Two rows of room for two rows of growth are not enough: one to spare.
        let mut room = vec![String::new(); 2];
        room.extend(tight.iter().cloned());
        let prompt = parse_prompt_v2(&room).expect("a box");
        assert!(!text_fits(
            &prompt,
            &room,
            Pane::of(&room),
            col,
            LONG_TERM_TEXT
        ));
        room.insert(0, String::new());
        let prompt = parse_prompt_v2(&room).expect("a box");
        assert!(text_fits(
            &prompt,
            &room,
            Pane::of(&room),
            col,
            LONG_TERM_TEXT
        ));
        // The pane's height, never the rows a `tail=` read sent: the tight
        // dialog drawn in a pane with the room is estimated to fit, and the
        // roomy rows read as a pane no taller than the dialog are not.
        let prompt = parse_prompt_v2(&tight).expect("a box");
        let taller = Pane {
            cols: COLS,
            rows: tight.len() + 3,
        };
        assert!(text_fits(&prompt, &tight, taller, col, LONG_TERM_TEXT));
        let prompt = parse_prompt_v2(&room).expect("a box");
        let short = Pane {
            cols: COLS,
            rows: tight.len(),
        };
        assert!(!text_fits(&prompt, &room, short, col, LONG_TERM_TEXT));
    }
}

// ---- the table (choose_text) ---------------------------------------------

/// Single-select, every row of the table: a pristine field with no memory gets
/// the text; the field holding exactly the remembered text gets Enter; one
/// remembered but empty is handed over (a text is never written twice); every
/// other text is a person's.
#[test]
fn a_single_select_is_typed_then_entered_and_anything_else_is_the_persons() {
    for p in [LONG, SHORT] {
        let text = text_of(p);
        let word = p.word();
        let pristine = screen(f::QUESTION_283_FREE_TEXT_FOCUSED);
        let d = decided(&pristine, p, None);
        let (what, rule, subject, unproven) = did(&d);
        assert_eq!(what, Did::Type(0, p));
        assert_eq!(rule, p.rule_id());
        let question = dialog_of(&pristine).question().expect("a question").clone();
        assert!(subject.starts_with(&question.text), "{subject}");
        assert!(unproven.contains(word), "{unproven}");
        let Decision::Approve { guard, .. } = &d else {
            unreachable!()
        };
        assert_eq!(*guard, row_guard(&pristine[question.rows.start]));
        // From option 1 (c4-01), two rows down first — and no "moved off"
        // note: the free-text row is where the text goes.
        let first = screen(f::QUESTION_283_TABS_FIRST);
        let (what, _, _, unproven) = did(&decided(&first, p, None));
        assert_eq!(what, Did::Type(2, p));
        assert!(!unproven.contains("moved off"), "{unproven}");
        // From the chat row, one row up — the free-text row is not a row
        // the focus is "moved off" onto: no note.
        let chat = with_focus(&pristine, row_of(&pristine, "4. Chat about this"));
        let (what, _, _, unproven) = did(&decided(&chat, p, None));
        assert_eq!(what, Did::Type(-1, p));
        assert!(!unproven.contains("moved off"), "{unproven}");

        // Remembered and gone: never written twice.
        assert!(reason(&decided(&pristine, p, Some(p))).contains("gone"));

        // Its own text: Enter on the field.
        let own = rewrap_free_text(f::QUESTION_283_TYPED_LONG_TERM, text, 120);
        let (what, rule, _, _) = did(&decided(&own, p, Some(p)));
        assert_eq!(what, Did::Enter(0, AnswerTarget::OwnText(p)));
        assert_eq!(rule, p.rule_id());
        // …from option 2, two rows down to it.
        let two = with_focus(&own, row_of(&own, "2. Blue"));
        assert_eq!(
            did(&decided(&two, p, Some(p))).0,
            Did::Enter(1, AnswerTarget::OwnText(p))
        );

        // Everything else is a person's.
        for (rows, own) in [
            (screen(f::QUESTION_283_TYPED_PERSON), Some(p)),
            (own.clone(), None),
            (
                rewrap_free_text(
                    f::QUESTION_283_TYPED_LONG_TERM,
                    &format!("{text} Blue"),
                    120,
                ),
                Some(p),
            ),
            (screen(f::QUESTION_283_TYPED_LONG_TERM), Some(p)),
            (
                rewrap_free_text(f::QUESTION_283_TYPED_LONG_TERM, "2", 120),
                Some(p),
            ),
        ] {
            let d = decided(&rows, p, own);
            let why = reason(&d);
            assert!(why.contains("did not write"), "{own:?}: {why}: {rows:#?}");
        }
    }
    // Another priority's text, remembered: the word changed after it.
    let short_own = rewrap_free_text(f::QUESTION_283_TYPED_LONG_TERM, SHORT_TERM_TEXT, 120);
    let d = decided(&short_own, LONG, Some(SHORT));
    let why = reason(&d);
    assert!(
        why.contains("short-term") && why.contains("long-term"),
        "{why}"
    );
    let long_as_short = rewrap_free_text(f::QUESTION_283_TYPED_LONG_TERM, LONG_TERM_TEXT, 120);
    assert!(
        reason(&decided(&long_as_short, SHORT, Some(SHORT))).contains("did not write"),
        "the long-term text is not the short-term policy's own"
    );
}

/// Multi-select, every row of the table: a pristine, unchecked field gets the
/// text; the loop's own text, ticked, is submitted by its button with the
/// focus moved off the field first (Enter in the field unticks it, c3c);
/// everything else is handed over.
#[test]
fn a_multi_select_is_typed_then_submitted_by_its_button_never_in_the_field() {
    for p in [LONG, SHORT] {
        let text = text_of(p);
        let pristine = screen(f::QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED);
        assert_eq!(did(&decided(&pristine, p, None)).0, Did::Type(0, p));
        // From option 1 (c4-05), three rows down.
        let second = screen(f::QUESTION_283_TABS_SECOND);
        assert_eq!(did(&decided(&second, p, None)).0, Did::Type(3, p));

        // Own text, ticked, the focus in the field: one row down to the
        // button, then Enter there, never Enter here.
        let typed = rewrap_free_text(f::QUESTION_283_MULTISELECT_TYPED_LONG_TERM, text, 120);
        assert_eq!(free_text_of(&typed).checked, Some(true));
        let (what, rule, _, _) = did(&decided(&typed, p, Some(p)));
        assert_eq!(what, Did::Enter(1, AnswerTarget::Button));
        assert_eq!(rule, p.rule_id());
        let on_button = rewrap_free_text(f::QUESTION_283_MULTISELECT_TYPED_BUTTON, text, 120);
        assert_eq!(
            did(&decided(&on_button, p, Some(p))).0,
            Did::Enter(0, AnswerTarget::Button)
        );
        let tab = rewrap_free_text(f::QUESTION_283_TABS_SECOND_TYPED_BUTTON, text, 120);
        assert_eq!(
            did(&decided(&tab, p, Some(p))).0,
            Did::Enter(0, AnswerTarget::Button)
        );

        // Own text unticked (c3c's state): its answer would be none.
        let unticked = edited(&typed, "[✔]", "[ ]");
        assert_eq!(free_text_of(&unticked).checked, Some(false));
        assert_eq!(free_text_of(&unticked).text, text);
        assert!(reason(&decided(&unticked, p, Some(p))).contains("unticked"));
        // A checked option, over an empty field or the loop's own text.
        let checked = edited(&pristine, "1. [ ] Cheese", "1. [✔] Cheese");
        assert!(reason(&decided(&checked, p, None)).contains("option 1 is checked"));
        let checked_own = edited(&typed, "1. [ ] Cheese", "1. [✔] Cheese");
        assert!(reason(&decided(&checked_own, p, Some(p))).contains("option 1 is checked"));
        // A person's text, the loop's text not remembered, remembered and gone.
        let person = screen(f::QUESTION_283_MULTISELECT_TYPED_PERSON);
        assert!(reason(&decided(&person, p, Some(p))).contains("did not write"));
        assert!(reason(&decided(&typed, p, None)).contains("did not write"));
        assert!(reason(&decided(&pristine, p, Some(p))).contains("gone"));
        // A checked, empty field.
        let ticked_empty = edited(&pristine, "4. [ ] Type something", "4. [✔] Type something");
        assert!(
            matches!(decided(&ticked_empty, p, None), Decision::Escalate { .. }),
            "a checked field is a person's"
        );
    }
}

/// Claude Code 2.1.300, measured: a multi-select question at 80 columns as
/// the harness read it, the focus on option 1 and the free-text row pristine.
const MULTI_80_2_1_300: &str = include_str!("fixtures/claude-2.1.300-question-multi-80col.txt");

/// `rows` with its rules drawn `cols` cells wide: the same dialog in a pane of
/// that width (its other rows fit any width this is used at).
fn at_width(rows: &[String], cols: usize) -> Vec<String> {
    rows.iter()
        .map(|r| {
            if !r.is_empty() && r.chars().all(|c| c == '─') {
                "─".repeat(cols)
            } else {
                r.clone()
            }
        })
        .collect()
}

/// Under `long-term`, a multi-select text its dialog would draw over once the
/// focus leaves for `Submit` (Claude Code 2.1.300 at 80 columns, and at 26,
/// 30, 34 and 40) is never typed: the tab takes its recommended option, said.
/// Negative control: at 110 and 120 columns, where the same text was measured
/// typed and submitted, it is typed.
#[test]
fn a_multi_select_text_its_dialog_would_draw_over_is_never_typed() {
    let rows = screen(MULTI_80_2_1_300);
    let decide_at = |cols: usize| {
        let rows = at_width(&rows, cols);
        let prompt = parse_prompt_v2(&rows).expect("a box");
        let pane = Pane {
            cols: u16::try_from(cols).expect("cols"),
            rows: 50,
        };
        answer_question(&prompt, &rows, LONG, None, false, pane)
    };
    for cols in [80, 40] {
        let (what, rule, subject, _) = did(&decide_at(cols));
        assert_eq!(
            what,
            Did::Enter(0, AnswerTarget::Option(1)),
            "{cols}: the recommended option, never the text: {subject}"
        );
        assert_eq!(rule, RULE_ANSWER_LONG_TERM);
    }
    for cols in [110, 120] {
        assert_eq!(did(&decide_at(cols)).0, Did::Type(3, LONG), "{cols}");
    }
}

/// Claude Code 2.1.300, measured: a multi-select's free-text row holding the
/// long-term text, typed by hand, after `down` to `Submit`, at 39, 47 and 57
/// columns (the dialog's rows).
const REFLOW_2_1_300: [(u16, &str); 3] = [
    (
        39,
        include_str!(
            "../../../../aterm-phase/src/fixtures/claude-2.1.300-question-multi-reflow-39col.txt"
        ),
    ),
    (
        47,
        include_str!(
            "../../../../aterm-phase/src/fixtures/claude-2.1.300-question-multi-reflow-47col.txt"
        ),
    ),
    (
        57,
        include_str!(
            "../../../../aterm-phase/src/fixtures/claude-2.1.300-question-multi-reflow-57col.txt"
        ),
    ),
];

/// Every frame measured on Claude Code 2.1.300 (the long-term text typed into
/// a multi-select's free-text row, label column 9, then `down` to `Submit`):
/// `(columns, the column its text was drawn from, whether `Submit` was drawn
/// over the text's last row)`.
const REFLOW_MEASURED: [(u16, usize, bool); 43] = [
    (38, 8, false),
    (39, 8, true),
    (41, 8, false),
    (42, 8, false),
    (43, 8, true),
    (44, 8, true),
    (45, 8, false),
    (46, 8, false),
    (47, 8, false),
    (48, 8, true),
    (49, 8, true),
    (50, 8, false),
    (51, 8, false),
    (52, 9, false),
    (53, 9, false),
    (54, 9, false),
    (55, 9, false),
    (56, 9, true),
    (57, 9, true),
    (58, 9, false),
    (60, 9, false),
    (62, 9, false),
    (63, 9, true),
    (64, 9, true),
    (65, 9, false),
    (70, 9, false),
    (75, 9, false),
    (79, 9, false),
    (80, 9, true),
    (81, 9, true),
    (82, 9, false),
    (90, 9, false),
    (100, 9, false),
    (101, 9, true),
    (102, 9, true),
    (103, 9, false),
    (120, 9, false),
    (143, 9, false),
    (144, 9, false),
    (145, 9, true),
    (146, 9, true),
    (147, 9, false),
    (150, 9, false),
];

/// [`drawn_rows`]'s wrap, row by row as drawn: greedy at single spaces, a
/// row filled to its last cell leaving the next one starting with the space
/// between (counted, not drawn: each row is drawn trimmed).
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

/// [`drawn_rows`] gives the measured 2.1.300 frames' text rows where a
/// greedy wrap differs (57 columns), and [`unfocused_row`] gives back every
/// one of the 43 frames measured on 2.1.300: the column the text was drawn
/// from, its rows as drawn, and whether `Submit` was drawn over the text's
/// last row, where the rule never types it — and it types everywhere else.
/// The harness's own frames at 26, 30, 34 and 40 columns were drawn over.
/// Negative control: a word wider than the row keeps nothing.
#[test]
fn a_multi_select_text_keeps_its_rows_only_where_the_reflow_cannot_drop_one() {
    for (cols, capture) in REFLOW_2_1_300 {
        let rows = screen(capture);
        let seen = reflowed(&rows, LONG_TERM_TEXT);
        let row = unfocused_row(LONG_TERM_TEXT, cols, 9).expect("laid out");
        assert_eq!(seen.col, row.col, "{cols} columns");
        let wrapped = drawn_wrap(LONG_TERM_TEXT, row.drawn);
        assert_eq!(seen.drawn, wrapped[..seen.drawn.len()], "{cols} columns");
        assert_eq!(drawn_rows(LONG_TERM_TEXT, row.drawn), Some(wrapped.len()));
    }
    assert_ne!(
        wrapped_rows(LONG_TERM_TEXT, 57 - 9),
        drawn_rows(LONG_TERM_TEXT, 57 - 9),
        "the greedy wrap is not the renderer's"
    );
    for (cols, col, over) in REFLOW_MEASURED {
        let row = unfocused_row(LONG_TERM_TEXT, cols, 9).expect("laid out");
        assert_eq!(row.col, col, "{cols} columns: the column");
        assert_eq!(
            text_keeps_its_rows(LONG_TERM_TEXT, cols, 9),
            !over,
            "{cols} columns"
        );
    }
    for cols in [26, 30, 34, 40] {
        assert!(!text_keeps_its_rows(LONG_TERM_TEXT, cols, 9), "{cols}");
    }
    assert!(!text_keeps_its_rows("Decide yourselfx", 24, 9));
}

/// What a frame of a measured reflow shows of `text`, typed into a
/// multi-select's free-text row (`  4. [✔]…` or `  4. [✔] …`) before the
/// focus moved down to `Submit`.
struct Reflowed {
    /// The column the text was drawn from.
    col: usize,
    /// Its rows over the button, from that column.
    drawn: Vec<String>,
    /// Every row kept: `Submit` alone on its row, and the rows over it
    /// joining back with one space to the text.
    kept: bool,
}

/// [`Reflowed`] of a frame's `rows`.
fn reflowed(rows: &[String], text: &str) -> Reflowed {
    let first = rows
        .iter()
        .position(|r| r.starts_with("  4. ["))
        .expect("the free-text row");
    let byte = rows[first].find("Decide").expect("the text");
    let col = rows[first][..byte].chars().count();
    let button = first
        + rows[first..]
            .iter()
            .position(|r| r.trim_start_matches('❯').trim_start().starts_with("Submit"))
            .expect("the button");
    let drawn: Vec<String> = rows[first..button]
        .iter()
        .map(|r| {
            r.chars()
                .skip(col)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    let kept = rows[button] == "❯    Submit" && drawn.join(" ") == text;
    Reflowed { col, drawn, kept }
}

/// ATERM-KI-1015-1 F2, on Claude Code 2.1.302 (measured in the real-Claude
/// lab, both policy texts typed by hand into a multi-select's free-text row,
/// then `down` to `Submit`, at every width from 26 to 160 columns, and from
/// 270 to 300 (long-term) and 240 to 260 (short-term), across the width from
/// which the row no longer overflows the pane):
/// [`unfocused_row`], from the pane's width, the row's label column and the
/// text alone, gives back every frame — the column the text was drawn from
/// (8 or 9: the long-term text at 8 up to 51 columns, the short-term text up
/// to 50) and its rows as drawn — so the text is typed at every width at
/// which it kept every row, and at no width at which `Submit` was drawn over
/// it. The harness's own decision on the pristine dialog as measured: typed
/// at 37 and 41 columns, the recommended option at 80. And at each width
/// round 20 gave up at, the frame after `down` reads as the loop's own text,
/// ticked, the button focused: `Submit` is pressed.
#[test]
fn a_multi_select_text_is_typed_at_every_measured_width_it_keeps_its_rows() {
    let mut misjudged: Vec<(QuestionPriority, u16, bool)> = Vec::new();
    for (p, sweep) in [
        (LONG, f::QUESTION_MULTI_REFLOW_LONG_SWEEP_2_1_302),
        (SHORT, f::QUESTION_MULTI_REFLOW_SHORT_SWEEP_2_1_302),
    ] {
        let text = text_of(p);
        let frames = f::reflow_frames(sweep);
        let widths: Vec<u16> = frames.iter().map(|(cols, _)| *cols).collect();
        assert_eq!(widths, (26..=160).collect::<Vec<u16>>(), "every width");
        let mut cols_at = BTreeSet::new();
        for (cols, rows) in &frames {
            let seen = reflowed(rows, text);
            let row = unfocused_row(text, *cols, 9).expect("laid out");
            assert_eq!(seen.col, row.col, "{p:?} at {cols} columns: the column");
            let wrapped = drawn_wrap(text, row.drawn);
            assert_eq!(
                seen.drawn,
                wrapped[..seen.drawn.len()],
                "{p:?} at {cols} columns: the rows as drawn"
            );
            assert_eq!(row.laid_out, row.drawn + 2, "{p:?} at {cols} columns");
            cols_at.insert(row.col);
            if text_keeps_its_rows(text, *cols, 9) != seen.kept {
                misjudged.push((p, *cols, seen.kept));
            }
        }
        assert_eq!(cols_at, BTreeSet::from([8, 9]), "{p:?}: both columns seen");
    }
    // Wider, across the width from which the row's basis (the number, the
    // checkbox and the text's widest row) fits the pane: the column is the
    // label's, the text is laid out at its widest row and drawn short of the
    // pane's edge, and `Submit` was measured drawn over the text's last row
    // exactly where the drawn width wraps it onto one row more than the
    // laid-out one (283 and 284 columns for the long-term text, 250 and 251
    // for the short-term text).
    for (p, sweep, widths, over) in [
        (
            LONG,
            f::QUESTION_MULTI_REFLOW_LONG_WIDE_2_1_302,
            270..=300,
            [283, 284],
        ),
        (
            SHORT,
            f::QUESTION_MULTI_REFLOW_SHORT_WIDE_2_1_302,
            240..=260,
            [250, 251],
        ),
    ] {
        let text = text_of(p);
        let frames = f::reflow_frames(sweep);
        let seen_widths: Vec<u16> = frames.iter().map(|(cols, _)| *cols).collect();
        assert_eq!(seen_widths, widths.collect::<Vec<u16>>(), "every width");
        let mut drawn_over = Vec::new();
        for (cols, rows) in &frames {
            let seen = reflowed(rows, text);
            let row = unfocused_row(text, *cols, 9).expect("laid out");
            assert_eq!((seen.col, row.col), (9, 9), "{p:?} at {cols} columns");
            let wrapped = drawn_wrap(text, row.drawn);
            assert_eq!(
                seen.drawn,
                wrapped[..seen.drawn.len()],
                "{p:?} at {cols} columns: the rows as drawn"
            );
            assert_eq!(row.drawn, row.laid_out.min(usize::from(*cols) - 9));
            if text_keeps_its_rows(text, *cols, 9) != seen.kept {
                misjudged.push((p, *cols, seen.kept));
            }
            if !seen.kept {
                drawn_over.push(*cols);
            }
        }
        assert_eq!(drawn_over, over, "{p:?}: PRECONDITION: measured drawn over");
    }
    assert!(
        misjudged.is_empty(),
        "(priority, columns, kept as measured) the rule misjudges: {misjudged:?}"
    );
    // The two texts land in different columns at 51: the short-term text's
    // widest row there is a cell short of the pane, so its checkbox keeps
    // its space.
    let col_at_51 = |text: &str| unfocused_row(text, 51, 9).expect("laid out").col;
    assert_eq!(
        (col_at_51(LONG_TERM_TEXT), col_at_51(SHORT_TERM_TEXT)),
        (8, 9)
    );
    for (cols, capture) in f::QUESTION_MULTI_PRISTINE_2_1_302 {
        let rows = screen(capture);
        let field = free_text_of(&rows);
        assert!(field.pristine, "{cols}: PRECONDITION: pristine");
        assert_eq!(label_col(&field), 9, "{cols}");
        let prompt = parse_prompt_v2(&rows).expect("a box");
        let pane = Pane { cols, rows: 60 };
        let (what, _, subject, _) = did(&answer_question(&prompt, &rows, LONG, None, false, pane));
        if cols == 80 {
            assert_eq!(what, Did::Enter(0, AnswerTarget::Option(1)), "{subject}");
        } else {
            assert_eq!(what, Did::Type(3, LONG), "{cols} columns: {subject}");
        }
    }
    for (cols, capture) in f::QUESTION_MULTI_REFLOW_LONG_KEPT_2_1_302 {
        let rows = screen(capture);
        assert!(reflowed(&rows, LONG_TERM_TEXT).kept, "{cols}: PRECONDITION");
        assert_eq!(
            did(&decided(&rows, LONG, Some(LONG))).0,
            Did::Enter(0, AnswerTarget::Button),
            "{cols} columns"
        );
    }
}

/// The review of round 21's F2 lane: the read-back after `down` at every
/// width the text is typed at, not the nine long-term widths above alone.
/// Each frame of the four 2.1.302 reflow sweeps from 33 columns (under it
/// option 1 wraps) at which [`text_keeps_its_rows`] holds, both texts,
/// rebuilt whole (`fixtures::reflow_dialog`, which aterm-phase holds to
/// the nine measured dialogs), reads as the loop's own text, ticked, the
/// button focused: `Submit` is pressed — the text at column 8 and at
/// column 9, and at the widths on either side of each band of widths
/// `Submit` was drawn over (the nine long-term widths above among them).
#[test]
fn a_multi_select_text_reads_back_as_submit_at_every_swept_width_it_is_typed_at() {
    let template = f::QUESTION_MULTI_REFLOW_LONG_KEPT_2_1_302[1].1;
    let mut misread: Vec<(QuestionPriority, u16, String)> = Vec::new();
    for (p, sweeps, swept, edges) in [
        (
            LONG,
            [
                f::QUESTION_MULTI_REFLOW_LONG_SWEEP_2_1_302,
                f::QUESTION_MULTI_REFLOW_LONG_WIDE_2_1_302,
            ],
            (159, 137),
            &[
                37, 38, 41, 42, 45, 47, 50, 55, 58, 62, 65, 79, 82, 100, 103, 144, 147, 282, 285,
            ][..],
        ),
        (
            SHORT,
            [
                f::QUESTION_MULTI_REFLOW_SHORT_SWEEP_2_1_302,
                f::QUESTION_MULTI_REFLOW_SHORT_WIDE_2_1_302,
            ],
            (149, 130),
            &[
                33, 36, 38, 41, 44, 48, 52, 55, 58, 71, 74, 87, 90, 128, 131, 249, 252,
            ][..],
        ),
    ] {
        let text = text_of(p);
        let mut frames = 0;
        let mut typed = BTreeSet::new();
        let mut cols_at = BTreeSet::new();
        for (cols, frame) in sweeps.into_iter().flat_map(f::reflow_frames) {
            if cols < 33 {
                continue;
            }
            frames += 1;
            if !text_keeps_its_rows(text, cols, 9) {
                continue;
            }
            typed.insert(cols);
            let rows = f::reflow_dialog(template, &frame, cols);
            let seen = reflowed(&rows, text);
            assert!(seen.kept, "{p:?} at {cols} columns: PRECONDITION: kept");
            cols_at.insert(seen.col);
            let d = decided(&rows, p, Some(p));
            if !matches!(d, Decision::Approve { .. })
                || did(&d).0 != Did::Enter(0, AnswerTarget::Button)
            {
                misread.push((p, cols, format!("{d:?}")));
            }
        }
        assert_eq!(
            (frames, typed.len()),
            swept,
            "{p:?}: PRECONDITION: the frames from 33 columns, and those typed"
        );
        let missing: Vec<u16> = edges
            .iter()
            .copied()
            .filter(|c| !typed.contains(c))
            .collect();
        assert!(missing.is_empty(), "{p:?}: not typed at {missing:?}");
        assert_eq!(cols_at, BTreeSet::from([8, 9]), "{p:?}: both columns");
    }
    assert!(
        misread.is_empty(),
        "(priority, columns, decision) not read back as `Submit`: {misread:#?}"
    );
}

/// The fallbacks, told: a tab the text would not fit and the preview form take
/// their recommended option under the priority's rule, saying why, and a
/// multi-select's fallback survives the tab's second key. The review is
/// submitted when every question has an answer, handed over when one was
/// skipped.
#[test]
fn a_tab_the_text_cannot_go_to_is_answered_with_its_recommended_option_and_says_so() {
    let p = LONG;
    let tight = |capture: &str| -> Vec<String> {
        let rows = screen(capture);
        let top = rows
            .iter()
            .position(|r| !r.is_empty() && r.chars().all(|c| c == '─'))
            .expect("the top rule");
        let last = rows
            .iter()
            .rposition(|r| !r.trim().is_empty())
            .expect("a footer");
        rows[top..=last].to_vec()
    };
    let single = tight(f::QUESTION_283_FREE_TEXT_FOCUSED);
    let (what, rule, _, _) = did(&decided(&single, p, None));
    assert_eq!(what, Did::Enter(-2, AnswerTarget::Option(1)));
    assert_eq!(rule, RULE_ANSWER_LONG_TERM);
    // A multi-select: its recommended option toggled, then — checked by the
    // fallback — its button.
    let multi = tight(f::QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED);
    assert_eq!(
        did(&decided(&multi, p, None)).0,
        Did::Enter(-3, AnswerTarget::Option(1))
    );
    let toggled = edited(&multi, "1. [ ] Cheese", "1. [✔] Cheese");
    assert_eq!(
        did(&decided(&toggled, p, None)).0,
        Did::Enter(1, AnswerTarget::Button)
    );
    // Negative control: with room, the same tabs are typed.
    assert_eq!(
        did(&decided(
            &screen(f::QUESTION_283_FREE_TEXT_FOCUSED),
            p,
            None
        ))
        .0,
        Did::Type(0, p)
    );

    // The preview form.
    let preview = screen(f::QUESTION_283_PREVIEW);
    let (what, rule, _, _) = did(&decided(&preview, p, None));
    assert_eq!(what, Did::Enter(0, AnswerTarget::Option(1)));
    assert_eq!(rule, RULE_ANSWER_LONG_TERM);
    // A preview it cannot read whole is handed over, as ever.
    let notes = screen(f::QUESTION_283_PREVIEW_NOTES_OPEN);
    reason(&decided(&notes, p, None));

    // The review.
    for capture in [
        f::QUESTION_283_TABS_REVIEW,
        f::QUESTION_283_MULTISELECT_REVIEW,
    ] {
        let rows = screen(capture);
        let (what, rule, _, _) = did(&decided(&rows, p, None));
        assert_eq!(what, Did::Enter(0, AnswerTarget::ReviewSubmit));
        assert_eq!(rule, RULE_ANSWER_LONG_TERM);
    }
    // A skipped question is the person's under a text policy.
    let skipped = screen(f::QUESTION_REVIEW_UNANSWERED);
    reason(&decided(&skipped, p, None));

    // An unmarked destructive question is the person's here too.
    let rows = screen(f::QUESTION_DO_YOU_WANT_LIVE);
    assert!(matches!(decided(&rows, p, None), Decision::Approve { .. }));
    let drop = edited(
        &rows,
        "Run the migration now.",
        "Drop the staging table now.",
    );
    assert_r10_is_the_recommended_paths(&drop, p);
}

/// In a text mode an unmarked question naming a destructive act is never left
/// to the worker but decided as the recommended path decides it: its one
/// refusal, said as a fallback, or handed over where that path hands it over.
fn assert_r10_is_the_recommended_paths(rows: &[String], p: QuestionPriority) {
    let recommended = decided(rows, QuestionPriority::Recommended, None);
    match (recommended, decided(rows, p, None)) {
        (
            Decision::Approve {
                choice,
                guard,
                subject,
                ..
            },
            Decision::Approve {
                rule_id,
                choice: text_choice,
                guard: text_guard,
                subject: text_subject,
                unproven,
            },
        ) => {
            assert_eq!((text_choice, text_guard), (choice, guard));
            assert_eq!(rule_id, p.rule_id());
            assert!(text_subject.starts_with(&subject), "{text_subject}");
            assert!(unproven.is_some(), "the fallback said");
        }
        (Decision::Escalate { reason: want }, Decision::Escalate { reason: got }) => {
            assert_eq!(got, want);
        }
        (want, got) => panic!("R10 decided otherwise under {p:?}: {got:?}, not {want:?}"),
    }
}

/// `decide` hands a question to the priority its context names, with the
/// loop's memory and the pane's width — and `answer_questions = false`
/// still hands it over whatever the priority. The width falls back to the
/// widest row read.
#[test]
fn decide_answers_by_the_contexts_priority() {
    let rows = screen(f::QUESTION_283_FREE_TEXT_FOCUSED);
    let reading = aterm_phase::read(Some("claude"), &rows, None);
    let mut ctx = ApprovalCtx::new(
        std::path::PathBuf::from("/tmp/work"),
        None,
        501,
        None,
        std::sync::Arc::new(crate::supervise::policy::LiveProbe),
    );
    assert!(ctx.answer_questions, "answered by default");
    assert_eq!(ctx.question_priority, QuestionPriority::Recommended);
    assert_eq!(
        (
            ctx.question_own,
            ctx.question_fell_back,
            ctx.screen_cols,
            ctx.screen_rows
        ),
        (None, false, 0, 0)
    );
    let d = decide(&reading, &rows, &ctx);
    assert!(
        matches!(&d, Decision::Approve { rule_id, .. } if *rule_id == RULE_ANSWER_RECOMMENDED),
        "{d:?}"
    );
    ctx.question_priority = LONG;
    assert_eq!(did(&decide(&reading, &rows, &ctx)).0, Did::Type(0, LONG));
    // Too narrow a pane for the text: its recommended option instead.
    ctx.screen_cols = 16;
    assert_eq!(
        did(&decide(&reading, &rows, &ctx)).0,
        Did::Enter(-2, AnswerTarget::Option(1))
    );
    ctx.screen_cols = 0;
    // Too short a pane (its height, not the rows read): the same.
    ctx.screen_rows = 14;
    assert_eq!(
        did(&decide(&reading, &rows, &ctx)).0,
        Did::Enter(-2, AnswerTarget::Option(1))
    );
    ctx.screen_rows = 0;
    // A fallback the loop latched on this tab: kept to, whatever the pane.
    ctx.question_fell_back = true;
    let (what, _, _, _) = did(&decide(&reading, &rows, &ctx));
    assert_eq!(what, Did::Enter(-2, AnswerTarget::Option(1)));
    ctx.question_fell_back = false;
    ctx.answer_questions = false;
    reason(&decide(&reading, &rows, &ctx));
}

// ---- the recommended path, unchanged -------------------------------------

/// Every question fixture aterm-phase carries (2.1.282 and 2.1.283), each
/// with its focus moved onto every row it can hold, and the decision under
/// [`QuestionPriority::Recommended`] as one line: what the recommended
/// path decides, whole.
fn recommended_sweep(
    decide_one: &dyn Fn(&aterm_phase::PromptV2, &[String]) -> Decision,
) -> Vec<String> {
    let fixtures: &[(&str, &str)] = &[
        ("YES_NO", f::QUESTION_YES_NO),
        ("DO_YOU_WANT", f::QUESTION_DO_YOU_WANT),
        ("TABS_INCIDENT", f::QUESTION_TABS_INCIDENT),
        ("TABS_SECOND", f::QUESTION_TABS_SECOND),
        ("TABS_THIRD", f::QUESTION_TABS_THIRD),
        ("TABS_FOURTH", f::QUESTION_TABS_FOURTH),
        ("REVIEW", f::QUESTION_REVIEW),
        ("REVIEW_UNANSWERED", f::QUESTION_REVIEW_UNANSWERED),
        ("ONE", f::QUESTION_ONE),
        ("RECOMMENDED_SECOND", f::QUESTION_RECOMMENDED_SECOND),
        ("FREE_TEXT_FOCUSED", f::QUESTION_FREE_TEXT_FOCUSED),
        ("FREE_TEXT_TYPED", f::QUESTION_FREE_TEXT_TYPED),
        ("FREE_TEXT_TYPED_AWAY", f::QUESTION_FREE_TEXT_TYPED_AWAY),
        ("MULTISELECT", f::QUESTION_MULTISELECT),
        ("MULTISELECT_CHECKED", f::QUESTION_MULTISELECT_CHECKED),
        ("MULTISELECT_NEXT", f::QUESTION_MULTISELECT_NEXT),
        ("MULTISELECT_TYPED", f::QUESTION_MULTISELECT_TYPED),
        ("NO_RECOMMENDATION", f::QUESTION_NO_RECOMMENDATION),
        ("PREVIEW", f::QUESTION_PREVIEW),
        ("LONG_LABEL_80COL", f::QUESTION_LONG_LABEL_80COL),
        ("LONG_LABEL_180COL", f::QUESTION_LONG_LABEL_180COL),
        ("DECLINED_CANCEL", f::QUESTION_DECLINED_CANCEL),
        ("DECLINED_ESC", f::QUESTION_DECLINED_ESC),
        ("CHAT_FOCUSED", f::QUESTION_CHAT_FOCUSED),
        ("AFK", f::QUESTION_AFK),
        ("PLUGIN_NOTICE", f::QUESTION_PLUGIN_NOTICE),
        ("PANEL_WAITING", f::QUESTION_PANEL_WAITING),
        ("DO_YOU_WANT_LIVE", f::QUESTION_DO_YOU_WANT_LIVE),
        ("MULTISELECT_ALONE", f::QUESTION_MULTISELECT_ALONE),
        ("SCROLLED", f::QUESTION_SCROLLED),
        ("PREVIEW_LONG_LABEL", f::QUESTION_PREVIEW_LONG_LABEL),
        ("PREVIEW_REVISITED", f::QUESTION_PREVIEW_REVISITED),
        ("DEFERRED", f::QUESTION_DEFERRED),
        ("OVER_DRAFT", f::QUESTION_OVER_DRAFT),
        ("OVER_DRAFT_AFTER", f::QUESTION_OVER_DRAFT_AFTER),
        ("283_FREE_TEXT_FOCUSED", f::QUESTION_283_FREE_TEXT_FOCUSED),
        ("283_TYPED_LONG_TERM", f::QUESTION_283_TYPED_LONG_TERM),
        ("283_TYPED_SHORT_TERM", f::QUESTION_283_TYPED_SHORT_TERM),
        ("283_TYPED_PERSON", f::QUESTION_283_TYPED_PERSON),
        (
            "283_MULTISELECT_FREE_TEXT_FOCUSED",
            f::QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED,
        ),
        (
            "283_MULTISELECT_TYPED_LONG_TERM",
            f::QUESTION_283_MULTISELECT_TYPED_LONG_TERM,
        ),
        (
            "283_MULTISELECT_TYPED_BUTTON",
            f::QUESTION_283_MULTISELECT_TYPED_BUTTON,
        ),
        ("283_MULTISELECT_REVIEW", f::QUESTION_283_MULTISELECT_REVIEW),
        (
            "283_MULTISELECT_TYPED_PERSON",
            f::QUESTION_283_MULTISELECT_TYPED_PERSON,
        ),
        (
            "283_MULTISELECT_UNTICKED",
            f::QUESTION_283_MULTISELECT_UNTICKED,
        ),
        ("283_TABS_FIRST", f::QUESTION_283_TABS_FIRST),
        ("283_TABS_FIRST_DOWN", f::QUESTION_283_TABS_FIRST_DOWN),
        (
            "283_TABS_FIRST_FREE_TEXT_FOCUSED",
            f::QUESTION_283_TABS_FIRST_FREE_TEXT_FOCUSED,
        ),
        (
            "283_TABS_FIRST_TYPED_LONG_TERM",
            f::QUESTION_283_TABS_FIRST_TYPED_LONG_TERM,
        ),
        ("283_TABS_SECOND", f::QUESTION_283_TABS_SECOND),
        ("283_TABS_SECOND_DOWN", f::QUESTION_283_TABS_SECOND_DOWN),
        (
            "283_TABS_SECOND_DOWN_TWICE",
            f::QUESTION_283_TABS_SECOND_DOWN_TWICE,
        ),
        (
            "283_TABS_SECOND_FREE_TEXT_FOCUSED",
            f::QUESTION_283_TABS_SECOND_FREE_TEXT_FOCUSED,
        ),
        (
            "283_TABS_SECOND_TYPED_LONG_TERM",
            f::QUESTION_283_TABS_SECOND_TYPED_LONG_TERM,
        ),
        (
            "283_TABS_SECOND_TYPED_BUTTON",
            f::QUESTION_283_TABS_SECOND_TYPED_BUTTON,
        ),
        ("283_TABS_REVIEW", f::QUESTION_283_TABS_REVIEW),
        ("283_PREVIEW", f::QUESTION_283_PREVIEW),
        (
            "283_PREVIEW_CHAT_FOCUSED",
            f::QUESTION_283_PREVIEW_CHAT_FOCUSED,
        ),
        ("283_PREVIEW_NOTES_OPEN", f::QUESTION_283_PREVIEW_NOTES_OPEN),
    ];
    let mut out = Vec::new();
    for (name, text) in fixtures {
        let rows = screen(text);
        let Some(prompt) =
            parse_prompt_v2(&rows).filter(|p| p.kind == aterm_phase::PromptKind::Question)
        else {
            out.push(format!("{name}: no question"));
            continue;
        };
        out.push(format!("{name}: {:?}", decide_one(&prompt, &rows)));
        let Some(dialog) = prompt.question_dialog.as_ref() else {
            continue;
        };
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
            QuestionForm::Unknown { .. } => Vec::new(),
        };
        for at in focusable {
            let Some(row) = focus_row(dialog, at) else {
                continue;
            };
            let moved = with_focus(&rows, row);
            let Some(p) = parse_prompt_v2(&moved) else {
                out.push(format!("{name}@{at:?}: no box"));
                continue;
            };
            out.push(format!("{name}@{at:?}: {:?}", decide_one(&p, &moved)));
        }
    }
    out
}

/// The recommended path reads neither the loop's memory nor the pane's
/// width: the sweep over every question fixture and every focus it can hold
/// decides the same under `Recommended` whatever they are.
#[test]
fn the_recommended_decisions_ignore_memory_and_pane_width() {
    let at = |own: Option<QuestionPriority>, cols: u16| {
        move |p: &aterm_phase::PromptV2, rows: &[String]| {
            answer_question(
                p,
                rows,
                QuestionPriority::Recommended,
                own,
                false,
                Pane {
                    cols,
                    rows: rows.len(),
                },
            )
        }
    };
    let base = recommended_sweep(&at(None, 120));
    for own in [None, Some(LONG), Some(SHORT)] {
        for cols in [0, 20, 80, 500] {
            assert_eq!(recommended_sweep(&at(own, cols)), base, "{own:?} {cols}");
        }
    }
}

// ---- Tier-1: the text decider bound to SupervisorQuestionText -------------

type ModelState = std::collections::BTreeMap<&'static str, i64>;

/// A decision in the model's words at `Text = 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    /// Enter where the focus is (`HarnessEnter`).
    Enter,
    /// A move toward the chosen row first (`HarnessMove`) — whatever
    /// follows it, a text or an Enter.
    Move,
    /// The text written where the focus is (`HarnessType`).
    Type,
    /// Nothing keyed: the person's.
    Escalate,
}

fn verdict(d: &Decision) -> Verdict {
    match d {
        Decision::Approve {
            choice: Choice::Answer(a),
            ..
        } => match (a.steps(), a.typed()) {
            (0, Some(_)) => Verdict::Type,
            (0, None) => Verdict::Enter,
            _ => Verdict::Move,
        },
        Decision::Escalate { .. } => Verdict::Escalate,
        other => panic!("a decision the model has no word for: {other:?}"),
    }
}

/// What the model lets the loop do at `st`, the loop's own guards granted
/// (a person quiet, its last key taken, the queue empty): the decision part,
/// the pure decider's.
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
        model.action_enabled("HarnessType", &free),
    ) {
        (true, false, false) => Verdict::Enter,
        (false, true, false) => Verdict::Move,
        (false, false, true) => Verdict::Type,
        (false, false, false) => Verdict::Escalate,
        both => panic!("the model enables more than one key {both:?} at {st:?}"),
    }
}

/// The captures a question tab is drawn from: pristine with the focus in
/// the field, the text typed with the focus in the field, and (multi) the
/// text typed with the focus on the button.
struct Tab {
    pristine: &'static str,
    typed: &'static str,
    typed_button: Option<&'static str>,
}

/// Tab `tab`'s captures, single- or multi-select: c4's first tab and c1
/// (single), c3 and c4's second tab (multi).
fn tab_captures(tab: i64, multi: bool) -> Tab {
    match (multi, tab) {
        (false, 0) => Tab {
            pristine: f::QUESTION_283_TABS_FIRST_FREE_TEXT_FOCUSED,
            typed: f::QUESTION_283_TABS_FIRST_TYPED_LONG_TERM,
            typed_button: None,
        },
        (false, _) => Tab {
            pristine: f::QUESTION_283_FREE_TEXT_FOCUSED,
            typed: f::QUESTION_283_TYPED_LONG_TERM,
            typed_button: None,
        },
        (true, 0) => Tab {
            pristine: f::QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED,
            typed: f::QUESTION_283_MULTISELECT_TYPED_LONG_TERM,
            typed_button: Some(f::QUESTION_283_MULTISELECT_TYPED_BUTTON),
        },
        (true, _) => Tab {
            pristine: f::QUESTION_283_TABS_SECOND_FREE_TEXT_FOCUSED,
            typed: f::QUESTION_283_TABS_SECOND_TYPED_LONG_TERM,
            typed_button: Some(f::QUESTION_283_TABS_SECOND_TYPED_BUTTON),
        },
    }
}

/// Every focusable row of `rows`' dialog.
fn focusable(rows: &[String]) -> Vec<QuestionFocus> {
    match dialog_of(rows).form {
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
        other => panic!("not a select tab: {other:?}"),
    }
}

/// One drawing of a model read as the decider is given it: the rows, the
/// loop's memory of its text, its fallback latch, and the pane.
struct Drawing {
    rows: Vec<String>,
    own: Option<QuestionPriority>,
    fell_back: bool,
    pane: Pane,
}

impl Drawing {
    /// `rows` in a pane exactly their size, the loop remembering `own`.
    fn on(rows: Vec<String>, own: Option<QuestionPriority>) -> Self {
        let pane = Pane::of(&rows);
        Self {
            rows,
            own,
            fell_back: false,
            pane,
        }
    }

    /// The same drawing with the focus on `row`.
    fn focused(&self, row: usize) -> Self {
        Self {
            rows: with_focus(&self.rows, row),
            own: self.own,
            fell_back: self.fell_back,
            pane: self.pane,
        }
    }
}

/// A model read at `Text = 1`: `seen_tab`, `seen_focus`, `seen_typed`,
/// `seen_own`, `seen_exact`, `seen_fallback`.
type TextRead = (i64, i64, i64, i64, i64, i64);

/// The height of the dialog on `rows`, its top rule to the last row drawn:
/// a pane no taller than that has no room for a text to grow.
fn dialog_height(rows: &[String]) -> usize {
    let top = rows
        .iter()
        .position(|r| !r.is_empty() && r.chars().all(|c| c == '─'))
        .expect("the top rule");
    let last = rows
        .iter()
        .rposition(|r| !r.trim().is_empty())
        .expect("a footer");
    last - top + 1
}

/// Every drawing of a model read, with the loop's memory of its text: the
/// read's tab (`tabs` the review), its focus (0 the chosen row: on a text tab
/// the free-text row, or on a multi-select holding the loop's text its
/// button; on a fallback tab its recommended option 1; and 1 every other row
/// it can hold), and its field: pristine; the loop's own text (`own`,
/// remembered); a person's exact copy of the policy text (`exact`, not
/// remembered); or other text (`typed`): a person's `2`, and on a tab the loop
/// may have written on, `2`, the text plus a person's words and the text with
/// its head dropped, each with the loop remembering its text. A fallback tab
/// is drawn in a pane too short for the text, and full-sized with the loop's
/// fallback latch. Drawn from the 2.1.283 captures (`rewrap_free_text`).
fn drawn(read: TextRead, tabs: i64, multi: bool, p: QuestionPriority) -> Vec<Drawing> {
    let (tab, focus, typed, own, exact, fallback) = read;
    if tab == tabs {
        assert_eq!(
            (typed, own, exact, fallback),
            (0, 0, 0, 0),
            "a field on the review"
        );
        let rows = screen(if multi {
            f::QUESTION_283_MULTISELECT_REVIEW
        } else {
            f::QUESTION_283_TABS_REVIEW
        });
        return vec![Drawing::on(
            if focus == 0 {
                rows.clone()
            } else {
                with_focus(&rows, row_of(&rows, "2. Cancel"))
            },
            None,
        )];
    }
    let caps = tab_captures(tab, multi);
    let text = text_of(p);
    let typed_as = |t: &str, memory: Option<QuestionPriority>| {
        Drawing::on(rewrap_free_text(caps.typed, t, 120), memory)
    };
    // The screens of this field, each with the focus in the field (on the
    // button, for the loop's own text on a multi-select).
    let bases: Vec<Drawing> = match (typed, own, exact) {
        (0, 0, _) if fallback == 1 => {
            let rows = screen(caps.pristine);
            let short = Pane {
                cols: COLS,
                rows: dialog_height(&rows),
            };
            vec![
                Drawing {
                    rows: rows.clone(),
                    own: None,
                    fell_back: false,
                    pane: short,
                },
                Drawing {
                    fell_back: true,
                    ..Drawing::on(rows, None)
                },
            ]
        }
        (0, 0, _) => vec![Drawing::on(screen(caps.pristine), None)],
        (0, _, _) => {
            assert_eq!(fallback, 0, "the loop's own text on a tab that falls back");
            vec![Drawing::on(
                match caps.typed_button {
                    Some(button) => rewrap_free_text(button, text, 120),
                    None => rewrap_free_text(caps.typed, text, 120),
                },
                Some(p),
            )]
        }
        (_, _, 1) => vec![typed_as(text, None)],
        _ => {
            let mut v = vec![typed_as("2", None)];
            if fallback == 0 {
                let suffix = text.split_once(' ').expect("a word").1;
                v.push(typed_as("2", Some(p)));
                v.push(typed_as(&format!("{text} and mine"), Some(p)));
                v.push(typed_as(suffix, Some(p)));
            }
            v
        }
    };
    let chosen = if fallback == 1 {
        QuestionFocus::Option(1)
    } else if multi && own == 1 {
        QuestionFocus::Button
    } else {
        QuestionFocus::FreeText
    };
    let mut out = Vec::new();
    for base in bases {
        let dialog = dialog_of(&base.rows);
        let at_chosen = if dialog.focus() == chosen {
            base
        } else {
            base.focused(focus_row(&dialog, chosen).expect("the chosen row"))
        };
        if focus == 0 {
            out.push(at_chosen);
            continue;
        }
        let dialog = dialog_of(&at_chosen.rows);
        for at in focusable(&at_chosen.rows)
            .into_iter()
            .filter(|at| *at != chosen)
        {
            let row = focus_row(&dialog, at).expect("a focusable row");
            out.push(at_chosen.focused(row));
        }
    }
    out
}

/// A decider under test: a drawing in, its decision out.
type Decider<'a> = &'a dyn Fn(&Drawing) -> Decision;

/// Where a verdict was seen: a tab the text goes to, a tab that falls back
/// to its recommended option, the review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Seen {
    Text,
    Fallback,
    Review,
}

/// A bind's outcome: the verdicts seen and where, or every disagreement as
/// `(the model's, the decider's)`.
type Bound = Result<BTreeSet<(Seen, Verdict)>, BTreeSet<(Verdict, Verdict)>>;

/// The model's side of the bind, once per form: every read the model at
/// `Text = 1`, `Multi = multi` reaches with a fresh read, and what it lets
/// the loop do there ([`decision_of`]) — with its `Tabs`.
struct ModelReads {
    multi: bool,
    tabs: i64,
    reads: Vec<(TextRead, Verdict)>,
}

/// [`ModelReads`] of the committed model at `Text = 1`, `Multi = multi`: its
/// whole bounded space walked once, breadth first (the interpreter's own
/// successor function, its environment built once a state). The states met
/// are kept in a hash set: a `BTreeSet` of value vectors cost 40% of the
/// test's time.
fn model_reads(multi: bool) -> ModelReads {
    let committed = aterm_models_harness::supervisor_question_text_model();
    let model = aterm_spec::interp::with_consts(&committed, &[("Multi", i64::from(multi))]);
    let tabs = model
        .consts
        .iter()
        .find(|(n, _)| *n == "Tabs")
        .map(|(_, v)| *v)
        .expect("Tabs");
    for dial in ["Text", "Multi", "OwnTextRemembered", "EnterWhereTyped"] {
        assert!(model.consts.iter().any(|(n, _)| *n == dial), "no {dial}");
    }
    let key = |s: &ModelState| -> Vec<i64> { s.values().copied().collect() };
    let init = model.init_state();
    let mut seen = std::collections::HashSet::from([key(&init)]);
    let mut queue = VecDeque::from([init]);
    let mut judged = BTreeSet::new();
    let mut reads = Vec::new();
    while let Some(st) = queue.pop_front() {
        let env = model.eval_env(&st);
        for a in &model.actions {
            for next in model.successors_in(a, &env, &st) {
                if seen.insert(key(&next)) {
                    queue.push_back(next);
                }
            }
        }
        if st["fresh"] != 1 || st["seen_tab"] > tabs {
            continue;
        }
        let read = (
            st["seen_tab"],
            st["seen_focus"],
            st["seen_typed"],
            st["seen_own"],
            st["seen_exact"],
            st["seen_fallback"],
        );
        if judged.insert(read) {
            reads.push((read, decision_of(&model, &st)));
        }
    }
    ModelReads { multi, tabs, reads }
}

/// Each of `model`'s reads drawn ([`drawn`]), decided by `decider` under
/// `p` and compared with the model: the verdicts seen and where, or every
/// disagreement as `(the model's, the decider's)`.
fn bind(model: &ModelReads, p: QuestionPriority, decider: Decider<'_>) -> Bound {
    let mut verdicts = BTreeSet::new();
    let mut wrong = BTreeSet::new();
    for &(read, want) in &model.reads {
        let at = if read.0 == model.tabs {
            Seen::Review
        } else if read.5 == 1 {
            Seen::Fallback
        } else {
            Seen::Text
        };
        for drawing in drawn(read, model.tabs, model.multi, p) {
            let got = verdict(&decider(&drawing));
            if got == want {
                verdicts.insert((at, got));
            } else {
                wrong.insert((want, got));
            }
        }
    }
    if wrong.is_empty() {
        Ok(verdicts)
    } else {
        Err(wrong)
    }
}

/// `decision` with its answer rewritten to an Enter on `target`, the focus
/// moved there from where `dialog` has it (a negative control's decider).
fn entered_on(
    decision: Decision,
    dialog: &aterm_phase::QuestionDialog,
    target: AnswerTarget,
) -> Decision {
    let from = focus_position(dialog, dialog.focus()).expect("a focus");
    let to = focus_position(dialog, target_focus(target)).expect("the target's row");
    let (rule_id, guard, subject, unproven) = match decision {
        Decision::Approve {
            rule_id,
            guard,
            subject,
            unproven,
            ..
        } => (rule_id, guard, subject, unproven),
        Decision::Escalate { reason } => (RULE_ANSWER_LONG_TERM, String::new(), reason, None),
        Decision::Decline { .. } => unreachable!("a question is never declined"),
    };
    Decision::Approve {
        rule_id,
        choice: Choice::Answer(Answer::FocusEnter {
            steps: to - from,
            target,
        }),
        guard,
        subject,
        unproven,
    }
}

/// Tier-1 for `SupervisorQuestionText` (`Text = 1`, aterm-spec
/// `supervisor_question_text_model`): at every reachable state with a fresh
/// read, single- and multi-select, the real [`answer_question`] under each
/// text priority over [`drawn`] writes the text, presses Enter, moves, or
/// hands over exactly where `HarnessType`, `HarnessEnter`, `HarnessMove` or
/// nothing is enabled ([`decision_of`]). Every verdict occurs on a text tab;
/// Enter, a move and a hand-over on a fallback tab; Enter and a move on the
/// review.
///
/// Negative controls, each caught by this comparison: (1) a decider taking any
/// text in the field for its own (`OwnTextRemembered = 0`); (2) one trusting
/// its memory without comparing it to the field; (3) one pressing Enter on its
/// text in a multi-select's field (`EnterWhereTyped = 1`, which unticks it).
#[test]
fn tier1_text_mode_decider_chooses_only_what_the_model_enables() {
    let single = model_reads(false);
    let multiple = model_reads(true);
    let of = |multi: bool| if multi { &multiple } else { &single };
    for p in [LONG, SHORT] {
        let real = move |d: &Drawing| {
            let prompt =
                parse_prompt_v2(&d.rows).unwrap_or_else(|| panic!("no box: {:#?}", d.rows));
            answer_question(&prompt, &d.rows, p, d.own, d.fell_back, d.pane)
        };
        for multi in [false, true] {
            let verdicts = bind(of(multi), p, &real).unwrap_or_else(|wrong| {
                panic!("{p:?} multi={multi}: the real decider disagrees with the model: {wrong:?}")
            });
            for (at, v) in [
                (Seen::Text, Verdict::Enter),
                (Seen::Text, Verdict::Move),
                (Seen::Text, Verdict::Type),
                (Seen::Text, Verdict::Escalate),
                (Seen::Fallback, Verdict::Enter),
                (Seen::Fallback, Verdict::Move),
                (Seen::Fallback, Verdict::Escalate),
                (Seen::Review, Verdict::Enter),
                (Seen::Review, Verdict::Move),
            ] {
                assert!(
                    verdicts.contains(&(at, v)),
                    "{p:?} multi={multi}: no {v:?} on {at:?} ({verdicts:?})"
                );
            }
            assert!(
                !verdicts.contains(&(Seen::Fallback, Verdict::Type)),
                "{p:?} multi={multi}: a text on a tab that falls back"
            );

            // (1) The blind decider.
            let blind = move |d: &Drawing| {
                let own = match dialog_of(&d.rows).form {
                    QuestionForm::Single { free_text, .. }
                    | QuestionForm::Multi { free_text, .. }
                        if !free_text.pristine =>
                    {
                        Some(p)
                    }
                    _ => None,
                };
                real(&Drawing {
                    rows: d.rows.clone(),
                    own,
                    fell_back: d.fell_back,
                    pane: d.pane,
                })
            };
            let wrong = bind(of(multi), p, &blind).expect_err("the blind decider is caught");
            assert!(
                wrong.contains(&(Verdict::Escalate, Verdict::Enter)),
                "{p:?} multi={multi}: {wrong:?}"
            );

            // (2) The memory-trusting decider: its memory, not the field.
            let trusting = move |d: &Drawing| {
                let decision = real(d);
                let dialog = dialog_of(&d.rows);
                match &dialog.form {
                    QuestionForm::Single { free_text, .. }
                        if d.own == Some(p) && !free_text.pristine =>
                    {
                        entered_on(decision, &dialog, AnswerTarget::OwnText(p))
                    }
                    QuestionForm::Multi { free_text, .. }
                        if d.own == Some(p)
                            && !free_text.pristine
                            && free_text.checked == Some(true) =>
                    {
                        entered_on(decision, &dialog, AnswerTarget::Button)
                    }
                    _ => decision,
                }
            };
            let wrong =
                bind(of(multi), p, &trusting).expect_err("the memory-trusting decider is caught");
            assert!(
                wrong.contains(&(
                    Verdict::Escalate,
                    if multi { Verdict::Move } else { Verdict::Enter }
                )),
                "{p:?} multi={multi}: {wrong:?}"
            );
        }

        // (3) The multi-as-single decider: its own text entered in the field.
        let single_minded = move |d: &Drawing| {
            let decision = real(d);
            let dialog = dialog_of(&d.rows);
            match (&dialog.form, &decision) {
                (
                    QuestionForm::Multi { .. },
                    Decision::Approve {
                        choice:
                            Choice::Answer(Answer::FocusEnter {
                                target: AnswerTarget::Button,
                                ..
                            }),
                        ..
                    },
                ) if d.own == Some(p) => entered_on(decision, &dialog, AnswerTarget::OwnText(p)),
                _ => decision,
            }
        };
        let wrong = bind(of(true), p, &single_minded).expect_err("multi-as-single is caught");
        assert!(
            wrong.contains(&(Verdict::Move, Verdict::Enter)),
            "{p:?}: {wrong:?}"
        );
        // …and on a single-select it is the real decider: nothing to catch.
        assert!(bind(of(false), p, &single_minded).is_ok());
    }
}

/// THE TEXT DRAWN AGAINST ITS CHECKBOX (ATERM-KI-1015-1 F2, the reader's
/// half). 2.1.300 MEASURED at 47 columns: once the focus left the free-text
/// row for `Submit`, the loop's own long-term text was drawn one cell left of
/// its label column (`[✔]Decide`), every row kept — a width
/// [`text_keeps_its_rows`] types at. The dialog read unknown, and was handed
/// over with the text in ("a question aterm could not read whole: named a
/// question by its footer or its options alone …"); it is submitted by its
/// button. NEGATIVE CONTROLS: the same frame with a word of the text a
/// person's, or its row unticked, is handed over; and the 39-column frame,
/// `Submit` drawn over the text's last row, is never read whole.
#[test]
fn a_multi_select_text_drawn_against_its_checkbox_is_submitted_by_its_button() {
    let (cols, capture) = REFLOW_2_1_300[1];
    assert_eq!(cols, 47);
    assert!(
        text_keeps_its_rows(LONG_TERM_TEXT, cols, 9),
        "typed at {cols} columns"
    );
    let rows = screen(capture);
    assert!(rows.iter().any(|r| r.starts_with("  4. [✔]Decide")));
    assert_eq!(
        did(&decided(&rows, LONG, Some(LONG))).0,
        Did::Enter(0, AnswerTarget::Button)
    );
    let person = edited(&rows, "[✔]Decide yourself", "[✔]Decide myself");
    assert!(
        reason(&decided(&person, LONG, Some(LONG))).contains("did not write"),
        "a person's word"
    );
    let unticked = edited(&rows, "[✔]Decide", "[ ]Decide");
    assert_eq!(
        reason(&decided(&unticked, LONG, Some(LONG))),
        "the long-term policy text's row reads unticked: its answer would be none"
    );
    let (_, over) = REFLOW_2_1_300[0];
    assert!(
        reason(&decided(&screen(over), LONG, Some(LONG)))
            .starts_with("a question aterm could not read whole"),
    );
}

/// ATERM-KI-1016-1 (Questions): `Submit` drawn over the last row of the loop's own text, the
/// rest of that row glued to it as one word (2.1.300 MEASURED at 57 columns, `❯
/// Submitked.)`). The dialog read whole with the text a row short, and the hand-over said the
/// loop "did not write" the text it typed; it is a dialog aterm could not read whole, as the
/// 39-column frame (`❯    Submit asked.)`) is, with no key pressed in either.
#[test]
fn a_text_overdrawn_by_its_button_is_one_aterm_could_not_read_whole() {
    let (cols, capture) = REFLOW_2_1_300[2];
    assert_eq!(cols, 57);
    let rows = screen(capture);
    assert!(
        rows.iter().any(|r| r == "❯    Submitked.)"),
        "PRECONDITION: the residue glued to the button"
    );
    for (p, own) in [(LONG, Some(LONG)), (LONG, None), (SHORT, Some(LONG))] {
        let why = reason(&decided(&rows, p, own)).to_string();
        assert!(
            why.starts_with("a question aterm could not read whole")
                && !why.contains("did not write"),
            "{p:?}/{own:?}: {why}"
        );
    }
}

/// MUSE CODE'S QUESTION TAKES THE WORD'S TEXT AS A NOTE, AND ITS REVIEW IS
/// SENT (1.4.10, measured in the lab: the whole 276-byte long-term text
/// reached the model as `note`): under `long-term` / `short-term` the
/// decision is the note ([`Choice::Note`], the word's rule, guarded on the
/// question's own row — its head's clock ticks), and each read's next
/// keystroke is the one [`note_step`] names, as on Codex's: arrows onto `None
/// of the above` (a digit only moves the cursor there, but arrows are what
/// the step presses), Tab there, the text into the empty note, Enter only on
/// the note holding exactly the text the loop remembers writing. The Enter
/// draws the REVIEW, which is sent — Enter on the focused `Submit answers`,
/// guarded on the review's key row — only when it holds that one answer,
/// `Color: None of the above; note: <the text>`, and the loop remembers the
/// text. NEGATIVE CONTROLS: the note holding the text the loop does NOT
/// remember writing, or the loop's text gone (Esc closed the field), gets no
/// keystroke; a review under `recommended`, under the other word, of a note
/// the loop did not write, of several questions, of other words, or with its
/// cursor moved to `Interrupt turn` is a person's; and several questions or
/// a multi-select under `long-term` are not answered (no roles).
#[test]
fn muses_question_takes_the_text_as_a_note_and_its_review_is_sent() {
    use aterm_phase::muse::fixtures as u;
    let read = |text: &str| {
        let rows = screen(text);
        let p = aterm_phase::read(Some("muse"), &rows, None)
            .prompt
            .expect("the question");
        (p, rows)
    };
    let row = |rows: &[String], head: &str| {
        rows.iter()
            .find(|r| r.starts_with(head))
            .cloned()
            .unwrap_or_else(|| panic!("a row {head:?}"))
    };
    let (prompt, rows) = read(u::QUESTION);
    for p in [LONG, SHORT] {
        match answer_question(&prompt, &rows, p, None, false, Pane::of(&rows)) {
            Decision::Approve {
                rule_id,
                choice,
                guard,
                subject,
                ..
            } => {
                assert_eq!(rule_id, p.rule_id(), "{p:?}");
                assert_eq!(choice, Choice::Note { text: p }, "{p:?}");
                assert_eq!(
                    guard,
                    row_guard(&row(&rows, "  Which color should the probe use?"))
                );
                assert_eq!(
                    subject,
                    format!(
                        "Which color should the probe use? → None of the above (the {} policy \
                         text as its note)",
                        p.word()
                    )
                );
            }
            other => panic!("{p:?}: not answered: {other:?}"),
        }
    }
    // Its keystrokes, read by read.
    let (p, rows) = read(u::QUESTION);
    assert_eq!(
        note_step(&p, &rows, LONG, None),
        Ok(NoteStep::Move {
            down: true,
            guard: row_guard(&row(&rows, "  ⟩ 1. Red (Recommended)"))
        })
    );
    let (p, rows) = read(u::QUESTION_NONE);
    assert_eq!(
        note_step(&p, &rows, LONG, None),
        Ok(NoteStep::Open {
            guard: row_guard(&row(&rows, "  ⟩ 3. None of the above"))
        })
    );
    let (p, rows) = read(u::QUESTION_NOTE_OPEN);
    assert_eq!(
        note_step(&p, &rows, LONG, None),
        Ok(NoteStep::Type {
            guard: row_guard(&row(&rows, "      Note (optional): "))
        })
    );
    let (p, rows) = read(u::QUESTION_NOTE_LONG);
    assert_eq!(
        note_step(&p, &rows, LONG, Some(LONG)),
        Ok(NoteStep::Enter {
            guard: row_guard(&row(&rows, "      Note (optional): ")),
            whole: true,
        })
    );
    assert!(
        note_step(&p, &rows, LONG, None).is_err(),
        "the text the loop did not write"
    );
    let (p, rows) = read(u::QUESTION_NOTE_ESC);
    assert!(
        note_step(&p, &rows, LONG, Some(LONG)).is_err(),
        "the loop's text gone"
    );
    // The review.
    let submit = |text: &str, priority: QuestionPriority, own: Option<QuestionPriority>| {
        let (p, rows) = read(text);
        assert!(aterm_phase::muse::is_review(&p), "PRECONDITION: {text}");
        (
            answer_question(&p, &rows, priority, own, false, Pane::of(&rows)),
            rows,
        )
    };
    let (d, rows) = submit(u::QUESTION_NOTE_LONG_REVIEW, LONG, Some(LONG));
    match d {
        Decision::Approve {
            rule_id,
            choice,
            guard,
            ..
        } => {
            assert_eq!(rule_id, LONG.rule_id());
            assert_eq!(
                choice,
                Choice::Focus {
                    steps: 0,
                    label: "Submit answers".to_string()
                }
            );
            assert_eq!(
                guard,
                row_guard(&row(&rows, "  Review answers before submit"))
            );
        }
        other => panic!("not sent: {other:?}"),
    }
    // Each control fails the one check it is named for, the rest of the
    // review as the loop's own: the loop's long-term review with its cursor
    // moved onto `Interrupt turn` (and nothing else changed) is one.
    let long_review = screen(u::QUESTION_NOTE_LONG_REVIEW);
    let moved = edited(
        &edited(&long_review, "  ⟩ Submit answers", "    Submit answers"),
        "    Interrupt turn",
        "  ⟩ Interrupt turn",
    );
    for (what, rows, priority, own, says) in [
        (
            "recommended",
            long_review.clone(),
            QuestionPriority::Recommended,
            Some(LONG),
            "answers the supervisor did not give",
        ),
        (
            "the other word",
            long_review.clone(),
            SHORT,
            Some(SHORT),
            "other answers than the note",
        ),
        (
            "not the loop's",
            long_review.clone(),
            LONG,
            None,
            "a note the supervisor did not write",
        ),
        (
            "several questions",
            screen(u::QUESTION_MULTI_REVIEW),
            LONG,
            Some(LONG),
            "one question is not read",
        ),
        (
            "other words",
            screen(u::QUESTION_NOTE_REVIEW),
            LONG,
            Some(LONG),
            "other answers than the note",
        ),
        (
            "the cursor moved",
            moved,
            LONG,
            Some(LONG),
            "cursor was moved off",
        ),
    ] {
        let p = aterm_phase::read(Some("muse"), &rows, None)
            .prompt
            .expect("the review");
        assert!(aterm_phase::muse::is_review(&p), "PRECONDITION: {what}");
        let d = answer_question(&p, &rows, priority, own, false, Pane::of(&rows));
        assert!(
            matches!(&d, Decision::Escalate { reason } if reason.contains(says)),
            "{what}: {d:?}"
        );
    }
    for text in [u::QUESTION_MULTI, u::QUESTION_MULTISELECT] {
        let (p, rows) = read(text);
        let d = answer_question(&p, &rows, LONG, None, false, Pane::of(&rows));
        assert!(matches!(d, Decision::Escalate { .. }), "{text}: {d:?}");
    }
}
