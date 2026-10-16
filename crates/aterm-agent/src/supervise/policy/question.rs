// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The question answer: which key the supervisor presses on an agent's
//! question dialog (Claude Code's AskUserQuestion as
//! [`aterm_phase::QuestionDialog`] reads it, and Codex's `Question 1/1` by its
//! answers' roles): [`answer_question`], rule [`RULE_ANSWER_RECOMMENDED`],
//! under `[harness] answer_questions`
//! ([`super::approval::ApprovalCtx::answer_questions`], on by default).
//!
//! No Claude Code setting removes the dialog, so the harness answers it with
//! every option marked `(Recommended)` ([`aterm_phase::Recommended`]), or
//! option 1 when none is. Single-select and preview take the lowest marked
//! option; multi-select toggles every marked option, then `Next`/`Submit`; the
//! review takes `1. Submit answers`. [`AnswerTarget`] cannot name the
//! free-text row, the chat row, cancel, Esc or `n`. The focus moves one row at
//! a time, each move seen landing, and Enter is pressed guarded on the focused
//! row as drawn ([`Answer::FocusEnter`], never a digit). Codex's question is
//! answered by its `(Recommended)` answer, else the first, by digit
//! ([`answer_by_role`], [`PromptV2::recommended`]); its Esc is never pressed.
//!
//! A person's answer is theirs: a selected preview, free text, or a check
//! outside the recommended set escalates; a moved focus or a skipped review
//! question is answered only once the person has been quiet for
//! `human_grace_s`. Escalated, never answered: a form not read whole
//! ([`QuestionForm::Unknown`]), no visible focus, a question with no
//! recommendation naming a destructive act
//! ([`super::turn_end::DESTRUCTIVE_WORDS`]) unless exactly one option refuses
//! it ([`refuses`]), a focus more than [`MAX_QUESTION_FOCUS_STEPS`] rows from
//! the target, and an answer whose words say it is kept for every later ask
//! ([`kept`]).
//!
//! Under [`QuestionPriority`] `long-term`/`short-term` the supervisor chooses
//! no option: it writes the word's fixed text ([`LONG_TERM_TEXT`],
//! [`SHORT_TERM_TEXT`]; [`Answer::Type`]) into a pristine free-text row where
//! it fits the pane ([`text_fits`]) and, on a multi-select, keeps its rows
//! once the focus leaves ([`text_keeps_its_rows`]), then presses Enter once a
//! later read shows exactly that text ([`AnswerTarget::OwnText`]), so the
//! worker decides ([`choose_text`]). A tab it cannot write on is answered with
//! its recommended option, and stays so ([`falls_back`]).
//!
//! Codex's question (read by its answers' roles, no dialog) takes the word's
//! text as a NOTE under `None of the above` (`note_answer`, [`note_step`];
//! measured on 0.163.0): the focus moved onto that row with ARROWS — a digit
//! answers at once, before any note — then Tab opens its notes field, the
//! text is written into it, and Enter goes only once a read shows the field
//! holding exactly that text. Esc or Tab in the field clears it, so neither is
//! ever pressed there. A question that offers no notes, or whose field did
//! not open, is answered with its recommended answer, said
//! (`fall_back_to`); one whose field holds anything else is a person's.
//!
//! Pure: the reading and the rows in, a [`Decision`] out; the loop
//! (`approval_loop.rs`) adds the person's quiet, keys in flight, retries and
//! its memory of the text it wrote.

use aterm_phase::prompt::{PromptV2, Role};
use aterm_phase::{
    FreeText, QuestionDialog, QuestionFocus, QuestionForm, QuestionOption, QuestionText,
    Recommended,
};

use super::approval::{Answer, AnswerTarget, Choice, Decision, QuestionPriority};
use super::guard::row_guard;

/// The rule id of an answered question (the ledger's, the journal's).
pub const RULE_ANSWER_RECOMMENDED: &str = "answer-recommended@v1";

/// The `long-term` policy text ([`QuestionPriority::LongTerm`]), written into
/// a question's free-text row: the measured wording
/// ([`aterm_phase::prompt::fixtures::MEASURED_LONG_TERM_2_1_283`]) without its
/// commas, semicolons, hyphens, `then` and quotes, since a comma in a
/// multi-select answer reaches Claude CSV-quoted and every wrapped row must
/// join back with one space. `the_policy_texts_keep_their_rules` pins the
/// rules: ASCII, no digit, no `, ; " - \`, single spaces, words of at most 16
/// cells, at most 279 bytes (below the paste-placeholder size), no `key `.
pub const LONG_TERM_TEXT: &str = "Decide yourself: choose the option that best serves long term \
                                  correctness and total value even at the cost of short term \
                                  deliverables. Say which you chose and why in one line and \
                                  continue without asking again. (Answered by the aterm harness \
                                  by policy. The user was not asked.)";

/// The `short-term` policy text ([`QuestionPriority::ShortTerm`]), under
/// the same rules as [`LONG_TERM_TEXT`].
pub const SHORT_TERM_TEXT: &str = "Decide yourself: choose the option that gets the current \
                                   deliverable done soonest at acceptable risk. Say which you \
                                   chose and why in one line and continue without asking again. \
                                   (Answered by the aterm harness by policy. The user was not \
                                   asked.)";

/// Whether `rule_id` is a question dialog's answer — under any
/// [`QuestionPriority`] ([`QuestionPriority::rule_id`]). The loop routes a
/// question's decision by this, never by one rule's id.
#[must_use]
pub fn is_question_rule(rule_id: &str) -> bool {
    QuestionPriority::ALL
        .into_iter()
        .any(|p| p.rule_id() == rule_id)
}

/// The most rows the focus is moved before the Enter: the furthest real move is
/// option 1 to `Next` in a four-option multi-select, five.
pub const MAX_QUESTION_FOCUS_STEPS: u32 = 6;

/// How the answer's option was chosen: which the journal's `unproven` says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Chosen {
    /// Its own row ends `(Recommended)`.
    Label,
    /// A row that may be its label's wrap ends `(Recommended)`: those rows.
    Wrapped(Vec<usize>),
    /// None is marked: option 1.
    Unmarked,
}

impl Chosen {
    fn unproven(&self) -> String {
        match self {
            Chosen::Label => "answered with its recommended option".to_string(),
            Chosen::Wrapped(rows) => format!(
                "answered with its recommended option (its label wraps: read to row {})",
                rows.iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Chosen::Unmarked => "no option is marked (Recommended): option 1".to_string(),
        }
    }
}

/// The options a question answers with ([`answer_question`]'s "Which
/// option"): every `Label` one, else every `Wrapped` one, else option 1.
fn recommended(options: &[QuestionOption]) -> (Vec<u8>, Chosen) {
    let label: Vec<u8> = options
        .iter()
        .filter(|o| o.recommended == Recommended::Label)
        .map(|o| o.n)
        .collect();
    if !label.is_empty() {
        return (label, Chosen::Label);
    }
    let wrapped: Vec<(u8, usize)> = options
        .iter()
        .filter_map(|o| match o.recommended {
            Recommended::Wrapped { label_end } => Some((o.n, label_end)),
            _ => None,
        })
        .collect();
    if !wrapped.is_empty() {
        return (
            wrapped.iter().map(|(n, _)| *n).collect(),
            Chosen::Wrapped(wrapped.iter().map(|(_, r)| *r).collect()),
        );
    }
    (vec![1], Chosen::Unmarked)
}

/// The options the window answers a question with, by number ([`recommended`]),
/// for a controller's `recommended` on a multi-select (`aterm drive answer`),
/// which checks every one of them.
#[must_use]
pub fn recommended_set(options: &[QuestionOption]) -> Vec<u8> {
    recommended(options).0
}

/// Where `focus` stands in the dialog's focus order, from 0: the options,
/// then the free-text row, then the multi-select button, then the chat row;
/// the review's two rows. `None` for no focus, or a row the form has not.
pub fn focus_position(dialog: &QuestionDialog, focus: QuestionFocus) -> Option<i32> {
    let n = |options: &[QuestionOption]| i32::try_from(options.len()).ok();
    let option = |k: u8, options: &[QuestionOption]| {
        (1..=options.len())
            .contains(&usize::from(k))
            .then(|| i32::from(k) - 1)
    };
    match (&dialog.form, focus) {
        (QuestionForm::Single { options, .. }, QuestionFocus::Option(k))
        | (QuestionForm::Multi { options, .. }, QuestionFocus::Option(k))
        | (QuestionForm::Preview { options, .. }, QuestionFocus::Option(k)) => option(k, options),
        (QuestionForm::Single { options, .. }, QuestionFocus::FreeText)
        | (QuestionForm::Multi { options, .. }, QuestionFocus::FreeText)
        | (QuestionForm::Preview { options, .. }, QuestionFocus::Chat) => n(options),
        (QuestionForm::Single { options, .. }, QuestionFocus::Chat)
        | (QuestionForm::Multi { options, .. }, QuestionFocus::Button) => n(options).map(|n| n + 1),
        (QuestionForm::Multi { options, .. }, QuestionFocus::Chat) => n(options).map(|n| n + 2),
        (QuestionForm::Review { .. }, QuestionFocus::ReviewSubmit) => Some(0),
        (QuestionForm::Review { .. }, QuestionFocus::ReviewCancel) => Some(1),
        _ => None,
    }
}

/// The screen row the dialog draws `focus` on; `None` for no focus, or a
/// row the form has not.
pub fn focus_row(dialog: &QuestionDialog, focus: QuestionFocus) -> Option<usize> {
    let option =
        |options: &[QuestionOption], k: u8| options.iter().find(|o| o.n == k).map(|o| o.row);
    match (&dialog.form, focus) {
        (QuestionForm::Single { options, .. }, QuestionFocus::Option(k))
        | (QuestionForm::Multi { options, .. }, QuestionFocus::Option(k))
        | (QuestionForm::Preview { options, .. }, QuestionFocus::Option(k)) => option(options, k),
        (QuestionForm::Single { free_text, .. }, QuestionFocus::FreeText)
        | (QuestionForm::Multi { free_text, .. }, QuestionFocus::FreeText) => Some(free_text.row),
        (QuestionForm::Multi { button, .. }, QuestionFocus::Button) => Some(button.row),
        (QuestionForm::Single { chat, .. }, QuestionFocus::Chat)
        | (QuestionForm::Multi { chat, .. }, QuestionFocus::Chat)
        | (QuestionForm::Preview { chat, .. }, QuestionFocus::Chat) => Some(chat.row),
        (QuestionForm::Review { submit, .. }, QuestionFocus::ReviewSubmit) => Some(submit.row),
        (QuestionForm::Review { cancel, .. }, QuestionFocus::ReviewCancel) => Some(cancel.row),
        _ => None,
    }
}

/// The label of option `n` of a question tab's options; `None` on the
/// review, or for an option the form has not.
pub fn option_label(dialog: &QuestionDialog, n: u8) -> Option<&str> {
    match &dialog.form {
        QuestionForm::Single { options, .. }
        | QuestionForm::Multi { options, .. }
        | QuestionForm::Preview { options, .. } => {
            options.iter().find(|o| o.n == n).map(|o| o.label.as_str())
        }
        _ => None,
    }
}

/// The focus an [`AnswerTarget`] needs.
pub fn target_focus(target: AnswerTarget) -> QuestionFocus {
    match target {
        AnswerTarget::Option(n) => QuestionFocus::Option(n),
        AnswerTarget::Button => QuestionFocus::Button,
        AnswerTarget::ReviewSubmit => QuestionFocus::ReviewSubmit,
        AnswerTarget::OwnText(_) => QuestionFocus::FreeText,
    }
}

/// The widest of `rows` in cells (its characters: a question dialog's
/// rules run the pane's width), for a read that carried no width
/// ([`super::approval::ApprovalCtx::screen_cols`]).
#[must_use]
pub fn widest_row(rows: &[String]) -> u16 {
    let widest = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
    u16::try_from(widest).unwrap_or(u16::MAX)
}

/// The pane a question dialog is drawn in, as [`text_fits`] measures it: its
/// width in cells and its height in rows, the pane's (`text --json`'s
/// `dims`), not the rows a `tail=` read sent (the loop reads the last 40 rows
/// of a taller pane, [`super::approval::ApprovalCtx::screen_rows`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pane {
    /// Its width in cells.
    pub cols: u16,
    /// Its height in rows.
    pub rows: usize,
}

impl Pane {
    /// A pane exactly the size of `rows`: as tall as they are, as wide as
    /// the widest of them — what a read that carried no `dims` says.
    #[must_use]
    pub fn of(rows: &[String]) -> Self {
        Self {
            cols: widest_row(rows),
            rows: rows.len(),
        }
    }
}

/// The column a free-text row's text starts at: after its number (`❯ 3. `, the
/// number at column 2) and, on a multi-select, its checkbox (`[ ] `): 5, or 9
/// behind the checkbox, for a one-digit number. Every row a typed text wraps
/// onto starts there.
#[must_use]
pub fn label_col(free_text: &FreeText) -> usize {
    let digits = free_text.n.to_string().len();
    2 + digits + 2 + if free_text.checked.is_some() { 4 } else { 0 }
}

/// How many rows `text` wraps onto, greedily at single spaces, in rows `width`
/// cells wide; `None` when a word is wider than a row.
#[must_use]
pub fn wrapped_rows(text: &str, width: usize) -> Option<usize> {
    let mut rows = 1usize;
    let mut line = 0usize;
    for word in text.split(' ') {
        let w = word.chars().count();
        if w > width {
            return None;
        }
        if line == 0 {
            line = w;
        } else if line + 1 + w <= width {
            line += 1 + w;
        } else {
            rows += 1;
            line = w;
        }
    }
    Some(rows)
}

/// How much narrower than the pane, past the label column, a typed text is
/// estimated to wrap: the focused field wraps one or two cells short of the
/// pane's edge, so four keeps the estimate at or above the rows drawn.
const WRAP_MARGIN: usize = 4;

/// Why a tab is answered with its recommended option under a text priority
/// when [`text_fits`] says the text would not fit its pane.
const FIT: &str = "the answer would not fit the pane";

/// The same, on a later read of a tab the loop began that way
/// ([`choose_text`]: a fallback begun is kept to).
const FIT_KEPT: &str = "the answer would not fit the pane when this tab was begun";

/// Why a multi-select tab is answered with its recommended option under a
/// text priority when [`text_keeps_its_rows`] says the dialog would draw its
/// button over the text once the focus left the text's row.
const OVERDRAWN: &str = "the answer would not fit the pane: the dialog would draw its button over \
                         the text's last row once the focus left it";

/// How many cells wider than its drawn text Claude Code lays out a
/// multi-select's free-text row once the focus has left it
/// ([`text_keeps_its_rows`]): measured on 2.1.300, the one slack every
/// measured frame agrees with.
const UNFOCUSED_LAYOUT_SLACK: usize = 2;

/// Whether `answer`, decided under a text priority on `dialog`, is its
/// tab's recommended fallback ([`choose_text`]): an Enter on an option of a
/// single- or multi-select, or on a multi-select's button while its
/// free-text row holds nothing — keys the text path never decides (it
/// writes the text, then enters it or the button after it). The loop
/// latches a question whose tab was decided so, and the decider keeps that
/// tab on its recommended path ([`answer_question`]'s `fell_back`).
#[must_use]
pub fn falls_back(dialog: &QuestionDialog, answer: &Answer) -> bool {
    match (&dialog.form, answer.enter_target()) {
        (
            QuestionForm::Single { .. } | QuestionForm::Multi { .. },
            Some(AnswerTarget::Option(_)),
        ) => true,
        (QuestionForm::Multi { free_text, .. }, Some(AnswerTarget::Button)) => free_text.pristine,
        _ => false,
    }
}

/// Whether `text`, written into the pristine free-text row of the question on
/// `rows` (`prompt`, drawn in `pane`), is estimated to leave the dialog whole
/// on screen. The field wraps at `label_col` ([`label_col`]) in rows
/// `pane.cols − label_col − 4` wide ([`WRAP_MARGIN`]), and the dialog grows by
/// the rows past the first ([`wrapped_rows`]), upward when it sits at the
/// pane's foot. It fits when the dialog as drawn (the rule over its tab bar to
/// the last row under it) plus that growth plus one spare row is no taller than
/// the pane (`pane.rows`, never the row count of a `tail=` read). A dialog
/// outgrowing the pane would lose its head, so a tab that does not fit is
/// answered with its recommended option ([`choose_text`]). Only this tab's
/// growth is estimated.
#[must_use]
pub fn text_fits(
    prompt: &PromptV2,
    rows: &[String],
    pane: Pane,
    label_col: usize,
    text: &str,
) -> bool {
    let width = usize::from(pane.cols).saturating_sub(label_col + WRAP_MARGIN);
    let Some(wrapped) = wrapped_rows(text, width) else {
        return false;
    };
    let (title, footer) = prompt.span;
    let is_rule = |r: &String| {
        let t = r.trim();
        !t.is_empty() && t.chars().all(|c| c == '─')
    };
    let top = match title.checked_sub(1) {
        Some(above) if rows.get(above).is_some_and(is_rule) => above,
        _ => title,
    };
    let last = (footer..rows.len())
        .rev()
        .find(|&k| !rows[k].trim().is_empty())
        .unwrap_or(footer);
    let drawn = last.saturating_sub(top) + 1;
    // The growth (`wrapped - 1` rows) and one row to spare, in the pane.
    drawn + wrapped <= pane.rows
}

/// How many rows Claude Code's renderer draws `text` on in rows `width`
/// cells wide (Ink's `wrap-ansi` with `trim: false`): greedily at single
/// spaces, save that a row filled to its last cell leaves the next row
/// starting with the space between, which takes a cell though it is not
/// drawn. `None` when a word is wider than a row. Measured on 2.1.300, it
/// reproduces every unfocused multi-select free-text row read at 38 to 150
/// columns; [`wrapped_rows`] does not.
#[must_use]
pub fn drawn_rows(text: &str, width: usize) -> Option<usize> {
    let mut rows = 1usize;
    let mut line = 0usize;
    for (k, word) in text.split(' ').enumerate() {
        let w = word.chars().count();
        if w > width {
            return None;
        }
        if k > 0 {
            if line >= width {
                rows += 1;
                line = 0;
            }
            line += 1;
        }
        if line + w > width && w > 0 {
            rows += 1;
            line = 0;
        }
        line += w;
    }
    Some(rows)
}

/// Whether `text`, typed into a multi-select's free-text row whose text
/// starts at `label_col` ([`label_col`]) in a pane `cols` wide, keeps every
/// row of it once the focus leaves the row for the dialog's button, the move
/// the answer makes before its Enter ([`choose_text`]).
///
/// With the focus gone, Claude Code 2.1.300 draws the text from its label
/// column (52 columns and up) or one cell left of it (`[✔]Decide`: 51 and
/// down) to the pane's edge, on [`drawn_rows`] rows of that width, but lays
/// the row out on [`drawn_rows`] rows [`UNFOCUSED_LAYOUT_SLACK`] cells wider.
/// Where those are fewer, the button is drawn over the text's last row: no
/// question reads there, and the loop could not press `Submit` on a text no
/// read shows whole. This predicted all 43 frames measured from 38 to 150
/// columns. The column is not known before the text is typed, so the text
/// keeps its rows only where it does at both; elsewhere the tab takes its
/// recommended option.
#[must_use]
pub fn text_keeps_its_rows(text: &str, cols: u16, label_col: usize) -> bool {
    let at_label = usize::from(cols).saturating_sub(label_col);
    [at_label, at_label + 1].into_iter().all(|drawn| {
        match (
            drawn_rows(text, drawn),
            drawn_rows(text, drawn + UNFOCUSED_LAYOUT_SLACK),
        ) {
            (Some(drawn), Some(laid_out)) => drawn == laid_out,
            _ => false,
        }
    })
}

/// The destructive word a question with no recommended option names in its
/// question, its labels or its descriptions, lowercased like the
/// turn-end policy's own test.
fn destructive(question: &QuestionText, options: &[QuestionOption]) -> Option<&'static str> {
    let mut text = question.text.to_lowercase();
    for o in options {
        text.push('\n');
        text.push_str(&o.label.to_lowercase());
        text.push('\n');
        text.push_str(&o.description.to_lowercase());
    }
    super::turn_end::destructive_word(&text)
}

/// Whether an option's label refuses what its question asks: `No` itself, or
/// `No` then a separator (`No, keep it`, `No — leave it`, `No (skip)`), never
/// a word that starts with it (`None of these`, `Not now`, `Now`).
fn refuses(label: &str) -> bool {
    let l = label.trim().to_lowercase();
    l.strip_prefix("no")
        .is_some_and(|rest| rest.chars().next().is_none_or(|c| !c.is_alphanumeric()))
}

/// The one option that refuses a question that would `w`, naming no act
/// itself, or why none can answer it.
fn the_refusal<'a, T>(
    w: &str,
    options: &'a [T],
    label: impl Fn(&T) -> String,
) -> Result<&'a T, String> {
    let refusing: Vec<&T> = options
        .iter()
        .filter(|o| {
            let text = label(o);
            refuses(&text) && super::turn_end::destructive_word(&text.to_lowercase()).is_none()
        })
        .collect();
    match refusing.as_slice() {
        [one] => Ok(one),
        _ => Err(format!(
            "a question with no recommended option that would {w}, and no one option that \
             refuses it: a person answers that"
        )),
    }
}

/// Why the option labelled `label`, described `description`, is no answer
/// the harness gives: its words say the answer is kept for every later ask
/// ([`aterm_phase::prompt::settles_later`]). Claude Code asks its own
/// preferences in this dialog, unmarked, its standing answer first: the
/// model-switch question's `1. Switch automatically` (`Continue on <m> now,
/// and switch without asking from now on`) writes `switchModelsOnFlag` into
/// the user's settings. A person answers that.
fn kept(label: &str, description: &str) -> Option<String> {
    let said = if description.is_empty() {
        String::new()
    } else {
        format!(" ({description})")
    };
    aterm_phase::prompt::settles_later(&format!("{label}\n{description}")).then(|| {
        format!(
            "a question whose answer `{label}`{said} is kept for every later ask: a person \
             answers that"
        )
    })
}

/// The journal's `unproven` for an answer by the refusing option.
fn refused(w: &str) -> String {
    format!("no option is marked (Recommended) and the question would {w}: its one refusal")
}

/// Why the focus note is appended to `unproven`: the focus was on a row the
/// answer never chooses, and was moved off it first.
fn moved_off(focus: QuestionFocus) -> Option<&'static str> {
    match focus {
        QuestionFocus::FreeText => Some("the focus was on the free-text row: moved off it first"),
        QuestionFocus::Chat => Some("the focus was on the chat row: moved off it first"),
        QuestionFocus::ReviewCancel => Some("the focus was on the review's cancel: moved off it"),
        _ => None,
    }
}

/// The decision on one question dialog (module header). `prompt` is a box of
/// kind [`aterm_phase::PromptKind::Question`], `rows` the screen it was read
/// from (the press is guarded on the question's first row as drawn, and the
/// loop guards its Enter on the focused row). `priority` says how
/// ([`QuestionPriority::Recommended`]: [`choose`] and [`answer_by_role`]; a
/// text priority: [`choose_text`]), `own` which priority's text the loop
/// remembers writing into this question's field, `fell_back` whether the loop
/// already decided this tab on its recommended option ([`falls_back`]), and
/// `pane` the pane it is drawn in (for [`text_fits`]).
#[must_use]
pub fn answer_question(
    prompt: &PromptV2,
    rows: &[String],
    priority: QuestionPriority,
    own: Option<QuestionPriority>,
    fell_back: bool,
    pane: Pane,
) -> Decision {
    let Some(dialog) = prompt.question_dialog.as_ref() else {
        return match priority.text() {
            None => answer_by_role(prompt, rows),
            // Codex's: the word's text as a note under `None of the above`,
            // else its recommended answer, said under the word's own rule.
            Some(_) => note_answer(prompt, rows, priority, own, fell_back),
        };
    };
    if let QuestionForm::Unknown { why } = dialog.form {
        return Decision::escalate(format!("a question aterm could not read whole: {why}"));
    }
    let focus = dialog.focus();
    let Some(from) = focus_position(dialog, focus) else {
        return Decision::escalate("a question with no visible focus (no ❯ row read)");
    };
    let Some(guard) = dialog
        .guard_row()
        .and_then(|k| rows.get(k))
        .map(|r| row_guard(r))
    else {
        return Decision::escalate("a question whose question row is not on the rows read");
    };
    // The owner's standing ruling (2026-10-16): at Claude Code's security-alert
    // model switch, stay on the current model whatever the question priority —
    // never auto-downgrade — rather than escalate it to a person (its `Switch
    // automatically` option is `settles_later`, so the ordinary rule hands it
    // over). This runs ahead of the priority match, so a `long-term` /
    // `short-term` loop does not type a policy answer into it either.
    let chosen = match stay_on_model(dialog) {
        Some(stay) => Ok(stay),
        None => match priority.text() {
            None => choose(dialog).map(|(target, subject, unproven)| TextChosen {
                target,
                typed: false,
                subject,
                unproven,
            }),
            Some(text) => choose_text(
                prompt,
                dialog,
                rows,
                (priority, text),
                (own, fell_back),
                pane,
            ),
        },
    };
    let TextChosen {
        target,
        typed,
        subject,
        unproven,
    } = match chosen {
        Ok(chosen) => chosen,
        Err(why) => return Decision::escalate(why),
    };
    let Some(to) = focus_position(dialog, target_focus(target)) else {
        return Decision::escalate("a question whose chosen row is not in its focus order");
    };
    let steps = to - from;
    if steps.unsigned_abs() > MAX_QUESTION_FOCUS_STEPS {
        return Decision::escalate(format!(
            "a question whose focus is {} rows from the option to answer (at most \
             {MAX_QUESTION_FOCUS_STEPS} are moved)",
            steps.unsigned_abs()
        ));
    }
    // Moved off a row the answer never chooses — which the free-text row is
    // not, when it is the row a policy text goes to.
    let unproven = match moved_off(focus) {
        Some(note) if steps != 0 && target_focus(target) != QuestionFocus::FreeText => {
            format!("{unproven}; {note}")
        }
        _ => unproven,
    };
    let answer = if typed {
        Answer::Type {
            steps,
            text: priority,
        }
    } else {
        Answer::FocusEnter { steps, target }
    };
    Decision::Approve {
        rule_id: priority.rule_id(),
        choice: Choice::Answer(answer),
        guard,
        subject,
        unproven: Some(unproven),
    }
}

/// The fallback said, spelled once: a tab answered by its recommended option
/// under text word `word` because `why`: the subject `<subject> (recommended:
/// <why>)` (what `CHOSE` and the ledger name) and the journal's `policy <word>:
/// <why>: <how>`, where `subject` and `how` are the recommended path's own.
fn said_fallback(subject: &str, word: &str, why: &str, how: &str) -> (String, String) {
    (
        format!("{subject} (recommended: {why})"),
        format!("policy {word}: {why}: {how}"),
    )
}

/// A recommended-path decision taken under the text priority `priority`
/// instead, because `why`: ledgered under the word's own rule, the fallback
/// said as [`choose_text`] says its own ([`said_fallback`]). An escalation
/// stays one. Used for a dialog read by its answers' roles and by the loop's
/// window that cannot fence a typed answer (`Session::decide_box`).
pub(crate) fn fall_back_to(decision: Decision, priority: QuestionPriority, why: &str) -> Decision {
    match decision {
        Decision::Approve {
            choice,
            guard,
            subject,
            unproven,
            ..
        } => {
            let (subject, unproven) = said_fallback(
                &subject,
                priority.word(),
                why,
                &unproven.unwrap_or_default(),
            );
            Decision::Approve {
                rule_id: priority.rule_id(),
                choice,
                guard,
                subject,
                unproven: Some(unproven),
            }
        }
        other => other,
    }
}

/// What a question's decider chose: the row its keys go to, whether the
/// policy text is written there (`typed`: [`Answer::Type`]) or Enter is
/// pressed on it, what the ledger names, and the journal's `unproven`.
struct TextChosen {
    target: AnswerTarget,
    typed: bool,
    subject: String,
    unproven: String,
}

/// The policy text's decision on one tab (module header) under `priority`,
/// whose fixed text is `text`, the loop remembering `own` in the field:
///
/// | form | the tab as read | decision |
/// |---|---|---|
/// | single | field pristine, no memory | [`text_fits`]: write the text (the focus moved there first), else its recommended option, said |
/// | single | field pristine, memory | handed over: the text the supervisor wrote is gone |
/// | single | field holds exactly the text, memory of this priority | Enter on it ([`AnswerTarget::OwnText`]) |
/// | single | any other text | a person's: handed over |
/// | multi | an option checked | handed over: a text answer toggles none |
/// | multi | field pristine and unchecked, no memory | [`text_fits`] and [`text_keeps_its_rows`]: write the text, else its recommended option, said |
/// | multi | field holds exactly the text, memory, ticked | Enter on the button (the focus moved off the field first) |
/// | multi | own text, unticked | handed over |
/// | multi | anything else | a person's: handed over |
/// | preview | — | its recommended option, said: no free-text row |
/// | review | a question skipped | handed over |
/// | review | every question answered | `1. Submit answers` |
///
/// A memory of another priority's text (the word changed after writing) hands
/// the tab over. A destructive unmarked question is answered as the
/// recommended path answers it (its one refusal; handed over where none
/// refuses, and on a multi-select), said as a fallback: nobody is there to
/// approve the act. A multi-select tab that does not fit takes its recommended
/// option whatever it has checked.
///
/// A fallback begun is kept to (`fell_back`, latched by the loop from
/// [`falls_back`]): the rest of that tab stays on the recommended path
/// whatever a later read estimates, so the loop's own toggle is never read
/// back as a person's check.
fn choose_text(
    prompt: &PromptV2,
    dialog: &QuestionDialog,
    rows: &[String],
    (priority, text): (QuestionPriority, &str),
    (own, fell_back): (Option<QuestionPriority>, bool),
    pane: Pane,
) -> Result<TextChosen, String> {
    let word = priority.word();
    // The recommended option instead, and the journal says why.
    let fallback = |why: &str| -> Result<TextChosen, String> {
        let (target, subject, how) = choose(dialog)?;
        let (subject, unproven) = said_fallback(&subject, word, why, &how);
        Ok(TextChosen {
            target,
            typed: false,
            subject,
            unproven,
        })
    };
    let write = |question: &QuestionText| TextChosen {
        target: AnswerTarget::OwnText(priority),
        typed: true,
        subject: format!("{} → the {word} policy text", question.text),
        unproven: format!(
            "policy {word}: the worker decides: its policy text written into the free-text row"
        ),
    };
    let decided = |question: &QuestionText, target: AnswerTarget| TextChosen {
        target,
        typed: false,
        subject: format!("{} → decided by the worker ({word})", question.text),
        unproven: format!("policy {word}: the worker decides by the policy text in its field"),
    };
    // An unmarked question that names a destructive act is never left
    // to the worker: the recommended path answers it.
    let destructive_unmarked = |question: &QuestionText, options: &[QuestionOption]| {
        (recommended(options).1 == Chosen::Unmarked)
            .then(|| destructive(question, options))
            .flatten()
            .map(|w| {
                format!("a question with no recommended option that would {w} is not the worker's")
            })
    };
    let other_word = own.filter(|o| *o != priority).map(|o| {
        format!(
            "the supervisor wrote the {} policy text into this question, and the policy is now \
             {word}: a person answers it",
            o.word()
        )
    });
    let own_here = own == Some(priority);
    let gone = "the policy text the supervisor wrote is gone from its free-text row";
    let not_written = || begun("text in its free-text row the supervisor did not write");
    match &dialog.form {
        QuestionForm::Single {
            question,
            options,
            free_text,
            ..
        } => {
            if let Some(why) = other_word {
                return Err(why);
            }
            if let Some(why) = destructive_unmarked(question, options) {
                return fallback(&why);
            }
            if fell_back {
                return fallback(FIT_KEPT);
            }
            if free_text.pristine {
                if own_here {
                    return Err(gone.to_string());
                }
                if !text_fits(prompt, rows, pane, label_col(free_text), text) {
                    return fallback(FIT);
                }
                return Ok(write(question));
            }
            if own_here && free_text.text == text {
                return Ok(decided(question, AnswerTarget::OwnText(priority)));
            }
            Err(not_written())
        }
        QuestionForm::Multi {
            question,
            options,
            free_text,
            ..
        } => {
            if let Some(why) = other_word {
                return Err(why);
            }
            if let Some(why) = destructive_unmarked(question, options) {
                return fallback(&why);
            }
            if fell_back {
                return fallback(FIT_KEPT);
            }
            let checked = options.iter().find(|o| o.checked == Some(true));
            let text_only = |n: u8| {
                begun(&format!(
                    "option {n} is checked, and the {word} policy answers this question with \
                     text only"
                ))
            };
            if free_text.pristine {
                if own_here {
                    return Err(gone.to_string());
                }
                if free_text.checked == Some(true) {
                    return Err(begun("its free-text row is checked"));
                }
                if !text_fits(prompt, rows, pane, label_col(free_text), text) {
                    return fallback(FIT);
                }
                if !text_keeps_its_rows(text, pane.cols, label_col(free_text)) {
                    return fallback(OVERDRAWN);
                }
                if let Some(o) = checked {
                    return Err(text_only(o.n));
                }
                return Ok(write(question));
            }
            if own_here && free_text.text == text {
                if let Some(o) = checked {
                    return Err(text_only(o.n));
                }
                if free_text.checked != Some(true) {
                    return Err(format!(
                        "the {word} policy text's row reads unticked: its answer would be none"
                    ));
                }
                return Ok(decided(question, AnswerTarget::Button));
            }
            Err(not_written())
        }
        QuestionForm::Preview { .. } => fallback("a preview question has no free-text row"),
        QuestionForm::Review { unanswered, .. } => {
            if let Some(open) = review_open(dialog, *unanswered) {
                return Err(format!(
                    "a review with a question unanswered ({open}): under the {word} policy a \
                     person submits it"
                ));
            }
            choose(dialog).map(|(target, subject, unproven)| TextChosen {
                target,
                typed: false,
                subject,
                unproven,
            })
        }
        QuestionForm::Unknown { why } => {
            Err(format!("a question aterm could not read whole: {why}"))
        }
    }
}

/// Why a question read by its answers' roles is answered with its
/// recommended answer under a text word: its notes field did not open when
/// the loop pressed Tab on `None of the above` (the loop's latch,
/// [`super::approval::ApprovalCtx::question_fell_back`]).
pub const NOTES_UNREAD: &str = "its notes field did not read open after Tab";

/// The answer `None of the above` of a question read by its answers' roles
/// (anchor `codex.question.none`, the row [`Role::Other`] gives it): the one
/// a note goes under. Its index among the options.
fn none_of_the_above(prompt: &PromptV2) -> Option<usize> {
    let none = aterm_phase::anchor("codex.question.none");
    prompt
        .options
        .iter()
        .position(|o| o.role == Role::Other && o.label == none)
}

/// What a question read by its answers' roles is remembered by across reads
/// (the loop's memory of the note it wrote, its fallback latch): its title,
/// its question and its answers' labels. A question whose words differ is
/// another question.
#[must_use]
pub fn roles_question_key(prompt: &PromptV2) -> String {
    let labels: Vec<&str> = prompt.options.iter().map(|o| o.label.as_str()).collect();
    format!(
        "{}\n{}\n{}",
        prompt.title,
        prompt.description,
        labels.join("\n")
    )
}

/// The next keystroke of a note ([`note_step`]), with the guard of the row
/// showing the state it answers ([`row_guard`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteStep {
    /// `down` (or `up`) toward `None of the above`, guarded on the focused
    /// answer's row. Never a digit: one answers the question at once.
    Move { down: bool, guard: String },
    /// `tab` on the focused `None of the above`: its notes field opens.
    Open { guard: String },
    /// The word's text, written into the open, empty notes field.
    Type { guard: String },
    /// `enter` on the notes field showing exactly the word's text: the
    /// question answered `None of the above` with the text as its note.
    Enter { guard: String },
}

impl NoteStep {
    /// The keystroke as a hand-over reason names it.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            NoteStep::Move { .. } => "arrow",
            NoteStep::Open { .. } => "Tab",
            NoteStep::Type { .. } => "note",
            NoteStep::Enter { .. } => "Enter",
        }
    }
}

/// A note's next keystroke on a fresh read of a question read by its
/// answers' roles (Codex's), under text priority `priority`, the loop
/// remembering `own` written into it: with the notes field closed, the focus
/// moved onto `None of the above`, then Tab; on the open empty field, the
/// text; on the field holding exactly the text the loop wrote, Enter. `Err`
/// for anything else, where no keystroke is sound: the text the loop wrote
/// gone from the field (Esc and Tab clear it — a person's keys, never the
/// loop's), the field holding text the loop did not write, or open under
/// another answer.
pub fn note_step(
    prompt: &PromptV2,
    rows: &[String],
    priority: QuestionPriority,
    own: Option<QuestionPriority>,
) -> Result<NoteStep, String> {
    let Some(text) = priority.text() else {
        return Err("the recommended policy writes no note".to_string());
    };
    let Some(none) = none_of_the_above(prompt) else {
        return Err("a question with no `None of the above` to write a note under".to_string());
    };
    let Some(focus) = prompt.options.iter().position(|o| o.focused) else {
        return Err("a question with no visible focus".to_string());
    };
    let guard_of = |row: usize| {
        rows.get(row)
            .map(|r| row_guard(r))
            .ok_or_else(|| "a row of the question is not on the rows read".to_string())
    };
    let mine = own == Some(priority);
    let gone = "the policy text the supervisor wrote is gone from the notes field (Esc or Tab \
                clears it): a person answers it";
    match aterm_phase::codex::question_notes(rows) {
        None if mine => Err(gone.to_string()),
        None if focus == none => Ok(NoteStep::Open {
            guard: guard_of(prompt.options[none].row)?,
        }),
        None => Ok(NoteStep::Move {
            down: none > focus,
            guard: guard_of(prompt.options[focus].row)?,
        }),
        Some(_) if focus != none => Err(begun(&format!(
            "its notes field is open under `{}`",
            prompt.options[focus].label
        ))),
        Some(n) if n.text.is_empty() && mine => Err(gone.to_string()),
        Some(n) if n.text.is_empty() => Ok(NoteStep::Type {
            guard: guard_of(n.row)?,
        }),
        Some(n) if n.text == text && mine => Ok(NoteStep::Enter {
            guard: guard_of(n.row)?,
        }),
        Some(_) => Err(begun(
            "text in its notes field the supervisor did not write",
        )),
    }
}

/// A question read by its answers' roles (Codex's) under a text priority
/// (module header): the word's text as a note under `None of the above`
/// ([`Choice::Note`], carried out one keystroke at a time by the loop on
/// [`note_step`]). Answered with its recommended answer instead, said
/// ([`fall_back_to`]), while its notes field reads closed and: the loop's
/// Tab did not open it before ([`NOTES_UNREAD`], `fell_back`), the question
/// has no `None of the above`, its footer offers no notes (anchor
/// `codex.question.notes.tab`), or it has no recommended answer and names a
/// destructive act — nobody is there to approve the act, so the recommended
/// path answers it (its one refusal, or a person). With the field open no
/// digit is ever pressed (one would be typed into it): a field the latch
/// gave up on, or one [`note_step`] has no keystroke for, is a person's.
fn note_answer(
    prompt: &PromptV2,
    rows: &[String],
    priority: QuestionPriority,
    own: Option<QuestionPriority>,
    fell_back: bool,
) -> Decision {
    if prompt.head_off_screen {
        return Decision::escalate(format!(
            "a question whose head is off the screen: {}",
            prompt.title
        ));
    }
    let word = priority.word();
    let fallback = |why: &str| fall_back_to(answer_by_role(prompt, rows), priority, why);
    let open = aterm_phase::codex::question_notes(rows).is_some();
    if open && fell_back {
        return Decision::escalate(format!(
            "its notes field read open after the {word} policy fell back to the recommended \
             answer ({NOTES_UNREAD}): a person answers it"
        ));
    }
    if !open && own.is_none() {
        if fell_back {
            return fallback(NOTES_UNREAD);
        }
        if none_of_the_above(prompt).is_none() {
            return fallback("a question with no `None of the above` to write a note under");
        }
        let offers = prompt
            .footer
            .as_deref()
            .is_some_and(|f| f.contains(aterm_phase::anchor("codex.question.notes.tab")));
        if !offers {
            return fallback("a question whose footer offers no notes");
        }
        if prompt.recommended().is_none() {
            let mut text = format!("{}\n{}", prompt.title, prompt.description).to_lowercase();
            for o in &prompt.options {
                text.push('\n');
                text.push_str(&o.label.to_lowercase());
            }
            if let Some(w) = super::turn_end::destructive_word(&text) {
                return fallback(&format!(
                    "a question with no recommended option that would {w} is not the worker's"
                ));
            }
        }
    }
    if let Err(why) = note_step(prompt, rows, priority, own) {
        return Decision::escalate(why);
    }
    let Some(guard) = rows.get(prompt.span.0).map(|r| row_guard(r)) else {
        return Decision::escalate("a question whose title row is not on the rows read");
    };
    Decision::Approve {
        rule_id: priority.rule_id(),
        choice: Choice::Note { text: priority },
        guard,
        subject: format!(
            "{} → {} (the {word} policy text as its note)",
            prompt.description,
            aterm_phase::anchor("codex.question.none")
        ),
        unproven: Some(format!(
            "policy {word}: the worker decides: its policy text written as the note of `{}`",
            aterm_phase::anchor("codex.question.none")
        )),
    }
}

/// A question read by its answers' roles, not as a dialog (Codex's): the
/// answer marked `(Recommended)`, else the first, by its digit, guarded on
/// the dialog's title row. An unmarked one that names a destructive act is
/// answered by its one refusal, as a dialog's is. Escalated: no refusal, a
/// head off the screen, or no answer to choose.
fn answer_by_role(prompt: &PromptV2, rows: &[String]) -> Decision {
    let answers: Vec<&aterm_phase::Opt> = prompt
        .options
        .iter()
        .filter(|o| o.role == Role::Answer)
        .collect();
    let Some(&first) = answers.first() else {
        return Decision::escalate(
            "a question aterm could not read whole: no dialog, and no answer to choose",
        );
    };
    if prompt.head_off_screen {
        return Decision::escalate(format!(
            "a question whose head is off the screen: {}",
            prompt.title
        ));
    }
    let chosen = prompt.recommended();
    let mut how = if chosen.is_some() {
        Chosen::Label.unproven()
    } else {
        Chosen::Unmarked.unproven()
    };
    let mut o = chosen.unwrap_or(first);
    if chosen.is_none() {
        let mut text = format!("{}\n{}", prompt.title, prompt.description).to_lowercase();
        for o in &answers {
            text.push('\n');
            text.push_str(&o.label.to_lowercase());
        }
        if let Some(w) = super::turn_end::destructive_word(&text) {
            match the_refusal(w, &answers, |o| o.label.clone()) {
                Ok(refusal) => {
                    o = refusal;
                    how = refused(w);
                }
                Err(why) => return Decision::escalate(why),
            }
        }
    }
    if let Some(why) = kept(&o.label, "") {
        return Decision::escalate(why);
    }
    let Some(n) = o.n else {
        return Decision::escalate(format!("the answer `{}` has no number", o.label));
    };
    let Some(guard) = rows.get(prompt.span.0).map(|r| row_guard(r)) else {
        return Decision::escalate("a question whose title row is not on the rows read");
    };
    Decision::Approve {
        rule_id: RULE_ANSWER_RECOMMENDED,
        choice: Choice::Digit(n),
        guard,
        subject: format!("{} → {}", prompt.title, o.label),
        unproven: Some(how),
    }
}

/// A person has begun answering: theirs to finish.
pub(crate) fn begun(what: &str) -> String {
    format!("a question a person has begun answering ({what}): theirs to finish")
}

/// Claude Code's SECURITY-ALERT model switch, and the option that stays put.
///
/// When the stronger model's safeguards flag a message, Claude Code opens a
/// `Model switch` question offering to drop to a weaker model: `1. Switch
/// automatically` (`Continue on <m> now, and switch without asking from now
/// on`, which writes `switchModelsOnFlag=true`) and `2. Stay on <model>`
/// (`Stop here without switching, and ask me each time a message is flagged`,
/// which writes `false`, the default). The switch option is
/// [`aterm_phase::prompt::settles_later`], so the ordinary rule escalates the
/// whole dialog to a person ([`kept`]).
///
/// The owner ruled (2026-10-16) that at every such alert the harness STAYS on
/// the current model: it never auto-downgrades to keep a flagged turn going.
/// The stay option settles nothing (it restores the ask-each-time default), so
/// pressing it opts into no standing automation — it is the conservative
/// answer a person would otherwise give here (the history of this very
/// session, twice). `None` when the known shape is absent, or a person has begun
/// answering it (its free-text row is not pristine).
///
/// Scoped by the whole shape, not loose wording: the alert's own words
/// (`safeguards` … `flagged`) AND no destructive act named anywhere
/// ([`destructive`]) AND a non-settling stay option (`Stay on …` /
/// `… without switching`) AND the companion `Switch automatically` downgrade
/// option (a `settles_later` sibling). This is a text-shape heuristic, not
/// origin authentication: the agent authors every word of an `AskUserQuestion`,
/// which renders through this same dialog, so an exact lookalike can still
/// match. The rule is kept conservative against that — it fires only when no
/// destructive act is named ([`destructive`]) and only to press the non-settling
/// Stay option. A question without the companion, or one that names a
/// destructive act, falls through to baseline policy, which can escalate or
/// auto-answer; the genuine dialog names no destructive word, so its Stay press
/// is unaffected.
/// (A fully localized/reworded dialog that matches none of these falls through
/// to the baseline policy, where the downgrade option's own `settles_later`
/// wording still escalates it; only a reword that defeats that too — out of
/// this rule's reach — could change the baseline, as before this change.)
fn stay_on_model(dialog: &QuestionDialog) -> Option<TextChosen> {
    let QuestionForm::Single {
        question,
        options,
        free_text,
        ..
    } = &dialog.form
    else {
        return None;
    };
    if !free_text.pristine {
        return None;
    }
    let q = question.text.to_lowercase();
    if !(q.contains("safeguards") && q.contains("flagged")) {
        return None;
    }
    // The genuine safeguards dialog only offers to switch models: it names no
    // destructive act in its question or any option. A look-alike that DOES
    // name one (an agent authors every word of an `AskUserQuestion`, which
    // renders through this same dialog) is NOT auto-answered here — returning
    // `None` hands it to the baseline policy, whose destructive-word rule routes
    // a no-recommendation destructive question to a person. This keeps the Stay
    // auto-press from suppressing the escalation the baseline would give a
    // destructive-named question, and does not touch the real dialog (it names
    // no such word, so the owner's 2026-10-16 Stay ruling still applies).
    if destructive(question, options).is_some() {
        return None;
    }
    let stay = options.iter().find(|o| {
        let label = o.label.trim().to_lowercase();
        label.starts_with("stay on ")
            && o.description.to_lowercase().contains("without switching")
            && !aterm_phase::prompt::settles_later(&format!("{}\n{}", o.label, o.description))
    })?;
    // Require the known dialog's COMPANION auto-downgrade option — a sibling
    // `Switch automatically` row whose answer is `settles_later` (writes
    // `switchModelsOnFlag`). Without this sibling the special Stay rule does
    // not fire; baseline policy still decides whether to escalate or answer.
    let has_switch_companion = options.iter().any(|o| {
        o.n != stay.n
            && o.label.to_lowercase().contains("switch")
            && aterm_phase::prompt::settles_later(&format!("{}\n{}", o.label, o.description))
    });
    if !has_switch_companion {
        return None;
    }
    Some(TextChosen {
        target: AnswerTarget::Option(stay.n),
        typed: false,
        subject: format!("{} → {}", question.text, stay.label),
        unproven: "a security alert's model switch: stayed on the current model (owner ruling, \
                   2026-10-16)"
            .to_string(),
    })
}

/// The row to press Enter on, what the ledger names, and the journal's
/// `unproven` — or why the question is the person's.
fn choose(dialog: &QuestionDialog) -> Result<(AnswerTarget, String, String), String> {
    let no_mark_that_would = |question: &QuestionText, options: &[QuestionOption]| {
        destructive(question, options).map(|w| {
            format!("a question with no recommended option that would {w}: a person answers that")
        })
    };
    match &dialog.form {
        QuestionForm::Single {
            question,
            options,
            free_text,
            ..
        } => {
            if !free_text.pristine {
                return Err(begun("text in its free-text row"));
            }
            choose_one(question, options)
        }
        QuestionForm::Preview {
            question, options, ..
        } => {
            if let Some(o) = options.iter().find(|o| o.selected) {
                return Err(format!(
                    "a question a person has answered (option {} is ticked ✔): theirs",
                    o.n
                ));
            }
            choose_one(question, options)
        }
        QuestionForm::Multi {
            question,
            options,
            free_text,
            button,
            ..
        } => {
            if !free_text.pristine || free_text.checked == Some(true) {
                return Err(begun("text in its free-text row"));
            }
            let (marked, how) = recommended(options);
            if how == Chosen::Unmarked
                && let Some(why) = no_mark_that_would(question, options)
            {
                return Err(why);
            }
            if let Some(why) = options
                .iter()
                .filter(|o| marked.contains(&o.n))
                .find_map(|o| kept(&o.label, &o.description))
            {
                return Err(why);
            }
            if let Some(o) = options
                .iter()
                .find(|o| o.checked == Some(true) && !marked.contains(&o.n))
            {
                return Err(begun(&format!(
                    "option {} is checked, and it is not recommended",
                    o.n
                )));
            }
            if let Some(o) = options
                .iter()
                .find(|o| marked.contains(&o.n) && o.checked != Some(true))
            {
                return Ok((
                    AnswerTarget::Option(o.n),
                    format!("{} → toggle {}", question.text, o.label),
                    how.unproven(),
                ));
            }
            let checked: Vec<&str> = options
                .iter()
                .filter(|o| o.checked == Some(true))
                .map(|o| o.label.as_str())
                .collect();
            Ok((
                AnswerTarget::Button,
                format!(
                    "{} → {} ({})",
                    question.text,
                    button.label,
                    checked.join(", ")
                ),
                how.unproven(),
            ))
        }
        QuestionForm::Review { unanswered, .. } => {
            if let Some(open) = review_open(dialog, *unanswered) {
                return Ok((
                    AnswerTarget::ReviewSubmit,
                    format!("submit answers (unanswered: {open})"),
                    format!(
                        "a person moved past a question: submitted without its answer ({open})"
                    ),
                ));
            }
            let n = dialog.tabs.len();
            Ok((
                AnswerTarget::ReviewSubmit,
                format!("submit answers ({n} of {n} answered)"),
                "submitted the answers its review lists".to_string(),
            ))
        }
        QuestionForm::Unknown { why } => {
            Err(format!("a question aterm could not read whole: {why}"))
        }
    }
}

/// A single-select or preview question's answer ([`choose`]): its
/// recommended option (else its first), by number — or, unmarked and
/// destructive, its one refusal — with what the ledger names and the
/// journal's `unproven`.
fn choose_one(
    question: &QuestionText,
    options: &[QuestionOption],
) -> Result<(AnswerTarget, String, String), String> {
    let (marked, how) = recommended(options);
    let (o, unproven) = match destructive(question, options) {
        Some(w) if how == Chosen::Unmarked => (
            the_refusal(w, options, |o| format!("{}\n{}", o.label, o.description))?,
            refused(w),
        ),
        _ => (
            options
                .iter()
                .find(|o| o.n == marked[0])
                .ok_or_else(|| format!("a question with no option {}", marked[0]))?,
            how.unproven(),
        ),
    };
    if let Some(why) = kept(&o.label, &o.description) {
        return Err(why);
    }
    Ok((
        AnswerTarget::Option(o.n),
        format!("{} → {}", question.text, o.label),
        unproven,
    ))
}

/// A review's questions still unanswered, as its answer names them (their
/// headers, else `the headers not read`) — `None` when every one is
/// answered.
fn review_open(dialog: &QuestionDialog, unanswered: bool) -> Option<String> {
    let open: Vec<&str> = dialog
        .tabs
        .iter()
        .filter(|t| !t.answered)
        .map(|t| t.header.as_str())
        .collect();
    if !unanswered && open.is_empty() {
        return None;
    }
    Some(if open.is_empty() {
        "the headers not read".to_string()
    } else {
        open.join(", ")
    })
}

#[cfg(test)]
#[path = "question_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "question_text_tests.rs"]
mod text_tests;
