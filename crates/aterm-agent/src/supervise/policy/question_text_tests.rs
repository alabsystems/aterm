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
    RULE_ANSWER_LONG_TERM, RULE_ANSWER_SHORT_TERM, RULE_PROSE_LONG_TERM, RULE_PROSE_SHORT_TERM,
    decide,
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

/// The texts' rules (§4): ASCII only; no digit and none of `, ; " - \` (a
/// digit could select an option, a comma draws CSV quotes around a
/// multi-select answer, `send` reads `\n`); single spaces, none at an edge; no
/// word over 16 cells; at most 279 bytes (below the paste-placeholder size);
/// no `key ` (a log reader could take it for a verb). Every greedy wrap from 40
/// to 220 columns at label columns 5 and 9, focused (one cell short) or not,
/// joins back with one space to the text exactly, and so does the reader over
/// the dialog 2.1.283 draws with it typed, single- and multi-select, focused
/// and on the button, at 80, 120 and 200 columns. Negative control: the
/// original wording breaks the rules (commas and hyphens).
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
    assert_eq!((LONG_TERM_TEXT.len(), SHORT_TERM_TEXT.len()), (276, 243));
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
    // The words, their rules and their texts.
    assert_eq!(QuestionPriority::WORDS.len(), QuestionPriority::ALL.len());
    for (p, word) in QuestionPriority::ALL
        .into_iter()
        .zip(QuestionPriority::WORDS)
    {
        assert_eq!(p.word(), word);
        assert_eq!(QuestionPriority::parse(word), Some(p));
        assert!(is_question_rule(p.rule_id()), "{word}");
    }
    assert_eq!(QuestionPriority::Recommended.text(), None);
    assert_eq!(LONG.text(), Some(LONG_TERM_TEXT));
    assert_eq!(SHORT.text(), Some(SHORT_TERM_TEXT));
    assert_eq!(
        [LONG.rule_id(), SHORT.rule_id()],
        [RULE_ANSWER_LONG_TERM, RULE_ANSWER_SHORT_TERM]
    );
    assert_eq!(
        [LONG.rule_id(), SHORT.rule_id()],
        ["answer-long-term@v1", "answer-short-term@v1"]
    );
    assert_eq!(
        [
            QuestionPriority::Recommended.prose_rule_id(),
            LONG.prose_rule_id(),
            SHORT.prose_rule_id()
        ],
        [
            super::super::turn_end::RULE_ANSWER,
            RULE_PROSE_LONG_TERM,
            RULE_PROSE_SHORT_TERM
        ]
    );
    assert_eq!(
        [RULE_PROSE_LONG_TERM, RULE_PROSE_SHORT_TERM],
        ["prose-long-term@v1", "prose-short-term@v1"]
    );
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

/// THE SESSION'S WORD SET IS SPELLED ONCE, BY THE PARSER. `ask` and every
/// priority word are what `meta set questions` accepts; the server's `meta`
/// verb help (aterm-types) and the drive help (this crate) spell that set by
/// hand, so a word added to [`QuestionPriority::WORDS`] must reach both or the
/// help sends an operator to a refusal. Derived from the parser, no prose.
#[test]
fn the_shipped_help_spells_the_session_words_the_parser_accepts() {
    let words = std::iter::once("ask")
        .chain(QuestionPriority::WORDS)
        .collect::<Vec<_>>()
        .join("|");
    let meta = aterm_types::control_verbs::spec("meta").expect("meta is in the table");
    assert!(
        meta.detail
            .contains(&format!("`meta set questions {words}`")),
        "the meta help does not spell the parser's set {words}"
    );
    let help = crate::DRIVE_HELP
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let spellings: Vec<&str> = help.split("meta set questions ask|").skip(1).collect();
    assert!(!spellings.is_empty(), "the drive help spells the set once");
    for rest in spellings {
        let set = rest
            .split(|c: char| !(c.is_ascii_lowercase() || c == '|' || c == '-'))
            .next()
            .unwrap_or(rest);
        assert_eq!(
            format!("ask|{set}"),
            words,
            "the drive help spells the session words differently from the parser"
        );
    }
}

/// THE FIT ESTIMATE is never below what 2.1.283 draws: the measured text
/// typed into c1-04, c3-05, c4-04 and c4-09 (and c8-02's short-term text)
/// took three rows at the label column (5, or 9 behind the checkbox); the
/// estimate at the pane's 120 columns is at least that. The reworded texts
/// fit the measured 120x44 panes, pristine; the same dialog in a pane no
/// taller than itself does not (it would grow past its head), nor does a
/// pane narrower than a word. NEGATIVE CONTROL: wrapped at the pane's full
/// width with no margin, the measured c8-02 text is estimated at FEWER rows
/// than drawn — the margin is what keeps the estimate above the vendor.
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
        // The PANE's height, never the rows a `tail=` read sent: the tight
        // dialog read whole but drawn in a pane with the room is estimated
        // to fit, and the roomy rows read as a pane no taller than the
        // dialog are not.
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

/// Single-select, every row of the table: a pristine field with no memory
/// gets the text (the focus moved there first when it is elsewhere), under
/// the priority's own rule; the field holding exactly that text, which the
/// loop remembers writing, gets Enter on it; the same field remembered but
/// empty is handed over (a text is never written twice); and every other
/// text — a person's words, the policy text the loop does not remember
/// writing, the text plus more, the first wording, another priority's
/// text — is a person's.
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
        assert_eq!(
            subject,
            format!("{} → the {word} policy text", question.text)
        );
        assert_eq!(
            unproven,
            format!(
                "policy {word}: the worker decides: its policy text written into the free-text row"
            )
        );
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
        assert_eq!(
            reason(&decided(&pristine, p, Some(p))),
            "the policy text the supervisor wrote is gone from its free-text row"
        );

        // Its own text: Enter on the field.
        let own = rewrap_free_text(f::QUESTION_283_TYPED_LONG_TERM, text, 120);
        let (what, rule, subject, _) = did(&decided(&own, p, Some(p)));
        assert_eq!(what, Did::Enter(0, AnswerTarget::OwnText(p)));
        assert_eq!(rule, p.rule_id());
        assert_eq!(
            subject,
            format!("{} → decided by the worker ({word})", question.text)
        );
        // …from option 2, two rows down to it.
        let two = with_focus(&own, row_of(&own, "2. Blue"));
        assert_eq!(
            did(&decided(&two, p, Some(p))).0,
            Did::Enter(1, AnswerTarget::OwnText(p))
        );

        // Everything else is a person's.
        let begun = "a question a person has begun answering (text in its free-text row the \
                     supervisor did not write): theirs to finish";
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
            assert_eq!(reason(&decided(&rows, p, own)), begun, "{own:?}: {rows:#?}");
        }
    }
    // Another priority's text, remembered: the word changed after it.
    let short_own = rewrap_free_text(f::QUESTION_283_TYPED_LONG_TERM, SHORT_TERM_TEXT, 120);
    assert_eq!(
        reason(&decided(&short_own, LONG, Some(SHORT))),
        "the supervisor wrote the short-term policy text into this question, and the policy is \
         now long-term: a person answers it"
    );
    let long_as_short = rewrap_free_text(f::QUESTION_283_TYPED_LONG_TERM, LONG_TERM_TEXT, 120);
    assert!(
        reason(&decided(&long_as_short, SHORT, Some(SHORT))).contains("did not write"),
        "the long-term text is not the short-term policy's own"
    );
}

/// Multi-select, every row of the table: a pristine, unchecked field gets
/// the text; the field holding the loop's own text, ticked, is submitted by
/// its button — the focus moved off the field first (Enter in the field
/// unticks it, c3c) — and never by an Enter where the text is; own text
/// unticked, any checked option, a checked but empty field, a person's text
/// and the text remembered but gone are handed over.
#[test]
fn a_multi_select_is_typed_then_submitted_by_its_button_never_in_the_field() {
    for p in [LONG, SHORT] {
        let text = text_of(p);
        let word = p.word();
        let pristine = screen(f::QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED);
        assert_eq!(did(&decided(&pristine, p, None)).0, Did::Type(0, p));
        // From option 1 (c4-05), three rows down.
        let second = screen(f::QUESTION_283_TABS_SECOND);
        assert_eq!(did(&decided(&second, p, None)).0, Did::Type(3, p));

        // Own text, ticked, the focus in the field: one row DOWN to the
        // button, then Enter there — never Enter here.
        let typed = rewrap_free_text(f::QUESTION_283_MULTISELECT_TYPED_LONG_TERM, text, 120);
        assert_eq!(free_text_of(&typed).checked, Some(true));
        let (what, rule, subject, _) = did(&decided(&typed, p, Some(p)));
        assert_eq!(what, Did::Enter(1, AnswerTarget::Button));
        assert_eq!(rule, p.rule_id());
        assert_eq!(
            subject,
            "Which toppings do you want? → decided by the worker (".to_string() + word + ")"
        );
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
        assert_eq!(
            reason(&decided(&unticked, p, Some(p))),
            format!("the {word} policy text's row reads unticked: its answer would be none")
        );
        // A checked option, over an empty field or the loop's own text.
        let text_only = format!(
            "a question a person has begun answering (option 1 is checked, and the {word} \
             policy answers this question with text only): theirs to finish"
        );
        let checked = edited(&pristine, "1. [ ] Cheese", "1. [✔] Cheese");
        assert_eq!(reason(&decided(&checked, p, None)), text_only);
        let checked_own = edited(&typed, "1. [ ] Cheese", "1. [✔] Cheese");
        assert_eq!(reason(&decided(&checked_own, p, Some(p))), text_only);
        // A person's text, the loop's text not remembered, remembered and gone.
        let person = screen(f::QUESTION_283_MULTISELECT_TYPED_PERSON);
        assert!(reason(&decided(&person, p, Some(p))).contains("did not write"));
        assert!(reason(&decided(&typed, p, None)).contains("did not write"));
        assert_eq!(
            reason(&decided(&pristine, p, Some(p))),
            "the policy text the supervisor wrote is gone from its free-text row"
        );
        // A checked, empty field.
        let ticked_empty = edited(&pristine, "4. [ ] Type something", "4. [✔] Type something");
        assert!(
            matches!(decided(&ticked_empty, p, None), Decision::Escalate { .. }),
            "a checked field is a person's"
        );
    }
}

/// The fallback said on a question read by its roles ([`said_fallback`] is
/// every fallback's one spelling): Codex's question, no dialog, under a text
/// priority, is answered with its recommended digit, ledgered under the word's
/// rule, `<title> → <answer> (recommended: <why>)` and `policy <word>: <why>:
/// <how>`. Negative control: under `recommended` it is the recommended path's
/// own decision, unrewritten.
#[test]
fn a_question_read_by_its_roles_falls_back_said_as_every_fallback_is() {
    let rows = screen(aterm_phase::codex::fixtures::QUESTION);
    let reading = aterm_phase::read(Some("codex"), &rows, None);
    let prompt = reading.prompt.as_ref().expect("the question");
    assert!(
        prompt.question_dialog.is_none(),
        "PRECONDITION: by its roles"
    );
    let why = "a question read by its answers' roles has no free-text row the supervisor writes";
    for p in [LONG, SHORT] {
        match answer_question(prompt, &rows, p, None, false, Pane::of(&rows)) {
            Decision::Approve {
                rule_id,
                choice,
                subject,
                unproven,
                ..
            } => {
                assert_eq!(rule_id, p.rule_id(), "{p:?}");
                assert_eq!(choice, Choice::Digit(1), "{p:?}");
                assert_eq!(
                    subject,
                    format!(
                        "Question 1/1 (1 unanswered) → subtract (Recommended) (recommended: {why})"
                    ),
                    "{p:?}"
                );
                let unproven = unproven.unwrap_or_default();
                assert!(
                    unproven.starts_with(&format!("policy {}: {why}: ", p.word()))
                        && unproven.len() > format!("policy {}: {why}: ", p.word()).len(),
                    "{p:?}: {unproven}"
                );
            }
            other => panic!("{p:?}: not answered: {other:?}"),
        }
    }
    // Negative control.
    match answer_question(
        prompt,
        &rows,
        QuestionPriority::Recommended,
        None,
        false,
        Pane::of(&rows),
    ) {
        Decision::Approve {
            rule_id, subject, ..
        } => {
            assert_eq!(rule_id, QuestionPriority::Recommended.rule_id());
            assert!(!subject.contains("(recommended: "), "{subject}");
        }
        other => panic!("not answered: {other:?}"),
    }
}

/// The fallbacks, told: a tab the text would not fit, and the preview form
/// (no free-text row), are answered with their RECOMMENDED option under the
/// priority's rule, the subject and the journal saying why. On a
/// multi-select that does not fit, the recommended path keeps judging its
/// own checks (its toggle, then its button), so the fallback survives the
/// tab's second key. The review is submitted when every question has an
/// answer, and handed over when one was skipped. The destructive-question
/// rule holds in a text mode.
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
    let (what, rule, subject, unproven) = did(&decided(&single, p, None));
    assert_eq!(what, Did::Enter(-2, AnswerTarget::Option(1)));
    assert_eq!(rule, RULE_ANSWER_LONG_TERM);
    assert!(
        subject.ends_with("→ Red (Recommended) (recommended: the answer would not fit the pane)"),
        "{subject}"
    );
    assert_eq!(
        unproven,
        "policy long-term: the answer would not fit the pane: answered with its recommended \
         option; the focus was on the free-text row: moved off it first"
    );
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
    let (what, rule, subject, unproven) = did(&decided(&preview, p, None));
    assert_eq!(what, Did::Enter(0, AnswerTarget::Option(1)));
    assert_eq!(rule, RULE_ANSWER_LONG_TERM);
    assert!(
        subject.ends_with("(recommended: a preview question has no free-text row)"),
        "{subject}"
    );
    assert_eq!(
        unproven,
        "policy long-term: a preview question has no free-text row: answered with its \
         recommended option"
    );
    // A preview it cannot read whole is handed over, as ever.
    let notes = screen(f::QUESTION_283_PREVIEW_NOTES_OPEN);
    assert!(
        reason(&decided(&notes, p, None)).starts_with("a question aterm could not read whole"),
        "notes open"
    );

    // The review.
    for capture in [
        f::QUESTION_283_TABS_REVIEW,
        f::QUESTION_283_MULTISELECT_REVIEW,
    ] {
        let rows = screen(capture);
        let (what, rule, subject, _) = did(&decided(&rows, p, None));
        assert_eq!(what, Did::Enter(0, AnswerTarget::ReviewSubmit));
        assert_eq!(rule, RULE_ANSWER_LONG_TERM);
        assert!(subject.starts_with("submit answers ("), "{subject}");
    }
    let skipped = screen(f::QUESTION_REVIEW_UNANSWERED);
    assert!(
        reason(&decided(&skipped, p, None))
            .starts_with("a review with a question unanswered (Placement): under the long-term"),
        "a skipped question is the person's under a text policy"
    );

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

/// In a text mode: a question with no recommended option that names a
/// destructive act is never left to the worker (the policy text would let it
/// take the act) but decided as the recommended path decides it: its one
/// refusal pressed, said as a fallback under the word's rule; or, where the
/// recommended path hands it over (no one refusal, a multi-select), handed
/// over for the same reason.
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
            assert_eq!(
                text_subject,
                format!(
                    "{subject} (recommended: a question with no recommended option that would \
                     drop is not the worker's)"
                )
            );
            assert!(
                unproven.is_some_and(|u| u.starts_with(&format!("policy {}: ", p.word()))),
                "the fallback said"
            );
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
    let mut ctx = ApprovalCtx::new(std::path::PathBuf::from("/tmp/work"), None, 501, None);
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
    // Too short a PANE (its height, not the rows read): the same.
    ctx.screen_rows = 14;
    assert_eq!(
        did(&decide(&reading, &rows, &ctx)).0,
        Did::Enter(-2, AnswerTarget::Option(1))
    );
    ctx.screen_rows = 0;
    // A fallback the loop latched on this tab: kept to, whatever the pane.
    ctx.question_fell_back = true;
    let (what, _, subject, _) = did(&decide(&reading, &rows, &ctx));
    assert_eq!(what, Did::Enter(-2, AnswerTarget::Option(1)));
    assert!(
        subject
            .ends_with("(recommended: the answer would not fit the pane when this tab was begun)"),
        "{subject}"
    );
    ctx.question_fell_back = false;
    ctx.answer_questions = false;
    assert_eq!(
        reason(&decide(&reading, &rows, &ctx)),
        "answer_questions is off: a question is the person's to answer"
    );
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

/// FNV-1a over the sweep's lines, one newline after each.
fn digest(lines: &[String]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for line in lines {
        for b in line.bytes().chain(*b"\n") {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// The recommended path is unchanged, byte for byte: the sweep over every
/// question fixture and every focus it can hold produces the pinned 291 lines
/// and digest under `Recommended`, whatever the loop's memory and the pane's
/// width (neither is read on that path).
#[test]
fn the_recommended_decisions_are_byte_identical_to_mains() {
    const MEASURED_LINES: usize = 291;
    const MEASURED_DIGEST: u64 = 0xae81_e427_d272_936a;
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
    assert_eq!(
        (base.len(), digest(&base)),
        (MEASURED_LINES, MEASURED_DIGEST),
        "the recommended path's decisions moved:\n{}",
        base.join("\n")
    );
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
/// (a person quiet, its last key taken, the queue empty): the DECISION
/// part, the pure decider's.
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
/// whole bounded space walked once (the interpreter's own successor
/// function, its environment built once a state), breadth first.
///
/// The states met are kept in a hash set, membership being all the walk asks
/// of it: a `BTreeSet` of value vectors compared them element by element down
/// its tree on every successor, 40% of the test's samples (measured
/// 2026-10-06). The walk's order is the queue's, as before.
fn model_reads(multi: bool) -> ModelReads {
    let committed = aterm_spec::derive::supervisor_question_text_model();
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
/// `supervisor_question_text_model`), the decision: at every reachable state,
/// single- and multi-select, where the loop holds a fresh read, the real
/// [`answer_question`] under each text priority (over the 2.1.283 captures as
/// that read showed them, with the loop's memory and fallback latch,
/// [`drawn`]) writes the text exactly where `HarnessType` is enabled, presses
/// Enter exactly where `HarnessEnter` is, moves exactly where `HarnessMove`
/// is, and hands over everywhere else, the loop's own guards granted
/// ([`decision_of`]). Every verdict occurs for both forms on a text tab; Enter,
/// a move and a hand-over on a fallback tab; Enter and a move on the review.
///
/// Negative controls, each caught by this comparison: (1) a blind decider
/// taking any text in the field for its own (`OwnTextRemembered = 0`) presses
/// Enter where the model hands over; (2) a memory-trusting decider that never
/// compares its remembered text to the field presses Enter, or moves to the
/// button, where the model hands over; (3) a multi-as-single decider pressing
/// Enter on its text in a multi-select's field (`EnterWhereTyped = 1`, which
/// unticks it) presses Enter where the model moves.
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
