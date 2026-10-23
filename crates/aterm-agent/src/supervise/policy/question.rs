// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The question answer: which key the supervisor presses on an agent's question dialog (Claude Code's AskUserQuestion as [`aterm_phase::QuestionDialog`] reads it, and agy's and Muse Code's by their answers' roles): [`answer_question`], rule [`RULE_ANSWER_RECOMMENDED`], under `[harness] answer_questions` ([`super::approval::ApprovalCtx::answer_questions`], on by default).

use aterm_phase::prompt::{PromptV2, Role, Select};
use aterm_phase::{
    FreeText, QuestionDialog, QuestionFocus, QuestionForm, QuestionOption, QuestionText,
    Recommended,
};

use super::approval::{Answer, AnswerTarget, Choice, Decision, QuestionPriority};
use super::guard::row_guard;

/// The rule id of an answered question (the ledger's, the journal's).
pub const RULE_ANSWER_RECOMMENDED: &str = "answer-recommended@v1";

/// The `long-term` policy text ([`QuestionPriority::LongTerm`]), written into a question's free-text row: the measured wording ([`aterm_phase::prompt::fixtures::MEASURED_LONG_TERM_2_1_283`]) without its commas, semicolons, hyphens, `then` and quotes, since a comma in a multi-select answer reaches Claude CSV-quoted and every wrapped row must join back with one space.
pub const LONG_TERM_TEXT: &str = "Decide yourself: choose the option that best serves long term \
                                  correctness and total value even at the cost of short term \
                                  deliverables. Say which you chose and why in one line and \
                                  continue without asking again. (Answered by the aterm harness \
                                  by policy. The user was not asked.)";

/// The `short-term` policy text ([`QuestionPriority::ShortTerm`]), under the same rules as [`LONG_TERM_TEXT`].
pub const SHORT_TERM_TEXT: &str = "Decide yourself: choose the option that gets the current \
                                   deliverable done soonest at acceptable risk. Say which you \
                                   chose and why in one line and continue without asking again. \
                                   (Answered by the aterm harness by policy. The user was not \
                                   asked.)";

/// Whether `rule_id` is a question dialog's answer — under any [`QuestionPriority`] ([`QuestionPriority::rule_id`]).
#[must_use]
pub fn is_question_rule(rule_id: &str) -> bool {
    QuestionPriority::ALL
        .into_iter()
        .any(|p| p.rule_id() == rule_id)
}

/// The most rows the focus is moved before the Enter: the furthest real move is option 1 to `Next` in a four-option multi-select, five.
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

/// What a key's ledger row says where its option was chosen as marked `(Recommended)` ([`Chosen::Label`], and [`Chosen::Wrapped`] after it): Settings ▸ Harness's DECIDED FOR YOU reads it back (`crate::harness::stats`), so the two cannot drift.
pub const CHOSE_RECOMMENDED: &str = "answered with its recommended option";

/// What a key's ledger row says where no option was marked and the first was taken ([`Chosen::Unmarked`]), read back as [`CHOSE_RECOMMENDED`] is.
pub const CHOSE_UNMARKED: &str = "no option is marked (Recommended): option 1";

impl Chosen {
    fn unproven(&self) -> String {
        match self {
            Chosen::Label => CHOSE_RECOMMENDED.to_string(),
            Chosen::Wrapped(rows) => format!(
                "{CHOSE_RECOMMENDED} (its label wraps: read to row {})",
                rows.iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Chosen::Unmarked => CHOSE_UNMARKED.to_string(),
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

/// The options the window answers a question with, by number ([`recommended`]), for a controller's `recommended` on a multi-select (`aterm drive answer`), which checks every one of them.
#[must_use]
pub fn recommended_set(options: &[QuestionOption]) -> Vec<u8> {
    recommended(options).0
}

/// Where `focus` stands in the dialog's focus order, from 0: the options, then the free-text row, then the multi-select button, then the chat row; the review's two rows.
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

/// The screen row the dialog draws `focus` on; `None` for no focus, or a row the form has not.
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

/// The label of option `n` of a question tab's options; `None` on the review, or for an option the form has not.
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

/// The widest of `rows` in cells (its characters: a question dialog's rules run the pane's width), for a read that carried no width ([`super::approval::ApprovalCtx::screen_cols`]).
#[must_use]
pub fn widest_row(rows: &[String]) -> u16 {
    let widest = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
    u16::try_from(widest).unwrap_or(u16::MAX)
}

/// The pane a question dialog is drawn in, as [`text_fits`] measures it: its width in cells and its height in rows, the pane's (`text --json`'s `dims`), not the rows a `tail=` read sent (the loop reads the last 40 rows of a taller pane, [`super::approval::ApprovalCtx::screen_rows`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pane {
    /// Its width in cells.
    pub cols: u16,
    /// Its height in rows.
    pub rows: usize,
}

impl Pane {
    /// A pane exactly the size of `rows`: as tall as they are, as wide as the widest of them — what a read that carried no `dims` says.
    #[must_use]
    pub fn of(rows: &[String]) -> Self {
        Self {
            cols: widest_row(rows),
            rows: rows.len(),
        }
    }
}

/// The column a free-text row's text starts at: after its number (`❯ 3.`
#[must_use]
pub fn label_col(free_text: &FreeText) -> usize {
    let digits = free_text.n.to_string().len();
    2 + digits + 2 + if free_text.checked.is_some() { 4 } else { 0 }
}

/// How many rows `text` wraps onto, greedily at single spaces, in rows `width` cells wide; `None` when a word is wider than a row.
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

/// Whether `answer`, decided under a text priority on `dialog`, is its tab's recommended fallback ([`choose_text`]): an Enter on an option of a single- or multi-select, or on a multi-select's button while its free-text row holds nothing — keys the text path never decides (it writes the text, then enters it or the button after it).
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

/// Whether `text`, written into the pristine free-text row of the question on `rows` (`prompt`, drawn in `pane`), is estimated to leave the dialog whole on screen.
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
    drawn + wrapped <= pane.rows
}

/// The cells of a multi-select free-text row's checkbox as Claude Code draws
/// it, `[ ] ` or `[✔] ` with the space after the box: its basis in the row's
/// layout ([`unfocused_row`]).
const CHECKBOX_CELLS: usize = 4;

/// The cells left of a select row's number: the pointer column and the gap
/// after it ([`unfocused_row`]).
const POINTER_CELLS: usize = 2;

/// The width of each row Claude Code's renderer wraps `text` onto in rows
/// `width` cells wide (Ink's `wrap-ansi` with `trim: false`): greedily at
/// single spaces; a row keeps the space before the word that went on to the
/// next row (it counts in the row's width, drawn blank), and a row filled to
/// its last cell leaves the next row starting with the space between.
fn wrapped_widths(text: &str, width: usize) -> Option<Vec<usize>> {
    let mut rows = Vec::new();
    let mut line = 0usize;
    for (k, word) in text.split(' ').enumerate() {
        let w = word.chars().count();
        if w > width {
            return None;
        }
        if k > 0 {
            if line >= width {
                rows.push(line);
                line = 0;
            }
            line += 1;
        }
        if line + w > width && w > 0 {
            rows.push(line);
            line = 0;
        }
        line += w;
    }
    rows.push(line);
    Some(rows)
}

/// How many rows Claude Code's renderer draws `text` on in rows `width` cells wide ([`wrapped_widths`]).
#[must_use]
pub fn drawn_rows(text: &str, width: usize) -> Option<usize> {
    wrapped_widths(text, width).map(|rows| rows.len())
}

/// Where Claude Code draws a multi-select free-text row's text once the focus has left the row ([`unfocused_row`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnfocusedRow {
    /// The column the text is drawn from: the row's label column, or one cell left of it where the checkbox gave up its space (`[✔]Decide`).
    pub col: usize,
    /// The width the text is wrapped at as drawn: to the pane's edge.
    pub drawn: usize,
    /// The width the dialog lays the text out at: the rows it makes room for.
    pub laid_out: usize,
}

/// Where Claude Code draws `text`, typed into a multi-select's free-text row whose text starts at `label_col` while the row is pristine ([`label_col`]), in a pane `cols` wide, once the focus has left the row — derived from the frame alone: the pane's width, the row's geometry (its pointer and gap, its number, its checkbox) and the text.
#[must_use]
pub fn unfocused_row(text: &str, cols: u16, label_col: usize) -> Option<UnfocusedRow> {
    let cols = usize::from(cols);
    let number = label_col.checked_sub(CHECKBOX_CELLS + POINTER_CELLS)?;
    let widest = wrapped_widths(text, cols)?.into_iter().max()?;
    let basis = number + CHECKBOX_CELLS + widest;
    let row = basis.min(cols);
    let over = basis - row;
    let shrinkable = CHECKBOX_CELLS + widest;
    let edge = (2 * (number + CHECKBOX_CELLS) * shrinkable + shrinkable
        - 2 * CHECKBOX_CELLS * over)
        / (2 * shrinkable);
    let col = POINTER_CELLS + edge;
    let laid_out = row.checked_sub(edge)?;
    let drawn = laid_out.min(cols.checked_sub(col)?);
    Some(UnfocusedRow {
        col,
        drawn,
        laid_out,
    })
}

/// Whether `text`, typed into a multi-select's free-text row whose text starts at `label_col` ([`label_col`]) in a pane `cols` wide, keeps every row of it once the focus leaves the row for the dialog's button, the move the answer makes before its Enter ([`choose_text`]): whether the rows it is drawn on ([`UnfocusedRow::drawn`]) are no more than the dialog lays out for it ([`UnfocusedRow::laid_out`]).
#[must_use]
pub fn text_keeps_its_rows(text: &str, cols: u16, label_col: usize) -> bool {
    let Some(row) = unfocused_row(text, cols, label_col) else {
        return false;
    };
    match (drawn_rows(text, row.drawn), drawn_rows(text, row.laid_out)) {
        (Some(drawn), Some(laid_out)) => drawn == laid_out,
        _ => false,
    }
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
/// ([`aterm_phase::prompt::settles_later`], label and description), or its
/// whole label is a vendor's answer kept for good
/// ([`aterm_phase::prompt::kept_answer`]; an agent's option that only holds
/// one, `Enable selected suites`, is answered).
fn kept(label: &str, description: &str) -> Option<String> {
    let said = if description.is_empty() {
        String::new()
    } else {
        format!(" ({description})")
    };
    (aterm_phase::prompt::settles_later(&format!("{label}\n{description}"))
        || aterm_phase::prompt::kept_answer(label))
    .then(|| {
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

/// The decision on one question dialog (module header).
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
        // Muse Code's review of its answers: sent only when it holds the
        if aterm_phase::muse::is_review(prompt) {
            return review_answer(prompt, rows, priority, own);
        }
        if let Some(why) = muses_persons_question(prompt) {
            return Decision::escalate(why);
        }
        return match priority.text() {
            None if roles_question_notes(prompt, rows).is_some() => Decision::escalate(begun(
                "its note is open, which the recommended word never opens: Enter would send \
                     what it holds with the answer",
            )),
            None => answer_by_role(prompt, rows),
            Some(_) if is_muse_question(prompt) || prompt.select == Select::Digits => {
                note_answer(prompt, rows, priority, own, fell_back)
            }
            Some(_) => fall_back_to(
                answer_by_role(prompt, rows),
                priority,
                "a question read by its answers' roles has no free-text row the supervisor writes",
            ),
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

/// A recommended-path decision taken under the text priority `priority` instead, because `why`: ledgered under the word's own rule, the fallback said as [`choose_text`] says its own ([`said_fallback`]).
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
fn choose_text(
    prompt: &PromptV2,
    dialog: &QuestionDialog,
    rows: &[String],
    (priority, text): (QuestionPriority, &str),
    (own, fell_back): (Option<QuestionPriority>, bool),
    pane: Pane,
) -> Result<TextChosen, String> {
    let word = priority.word();
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

/// Why a question read by its answers' roles is answered with its recommended answer under a text word: its notes field did not open when the loop pressed Tab on `None of the above` (the loop's latch, [`super::approval::ApprovalCtx::question_fell_back`]).
pub const NOTES_UNREAD: &str = "its notes field did not read open after Tab";

/// The answer `None of the above` of a question read by its answers' roles
/// (Muse Code's `muse.question.none`, the row [`Role::Other`] gives it): the
/// one a note goes under.
fn none_of_the_above(prompt: &PromptV2) -> Option<usize> {
    let none = aterm_phase::anchor("muse.question.none");
    prompt
        .options
        .iter()
        .position(|o| o.role == Role::Other && o.label == none)
}

/// Muse Code's single question, by its key row (`muse.question.keys`, the one
/// whose answers carry roles): its note is Muse Code's
/// ([`aterm_phase::muse::question_notes`]), opened with Tab, and a note
/// sends it to its review ([`review_answer`]).
fn is_muse_question(prompt: &PromptV2) -> bool {
    prompt.footer.as_deref() == Some(aterm_phase::anchor("muse.question.keys"))
}

/// Why Muse Code's question is a person's by its key row (measured on
/// 1.4.10): a call of several questions (`muse.question.keys_multi`) or a
/// multi-select (`muse.question.keys_toggle`) is answered across screens —
/// each question, then a review whose second row stops the turn — on which
/// the loop cannot tell its own keys' answers from a person's (Left revisits
/// an answered question with no mark of its answer).
fn muses_persons_question(prompt: &PromptV2) -> Option<String> {
    let footer = prompt.footer.as_deref()?;
    let what = if footer == aterm_phase::anchor("muse.question.keys_multi") {
        "Muse Code's call of several questions"
    } else if footer == aterm_phase::anchor("muse.question.keys_toggle") {
        "Muse Code's multi-select question"
    } else {
        return None;
    };
    Some(format!(
        "{what} is answered across screens and a review the supervisor cannot tell its own keys' \
         answers from a person's on: a person's to answer ({})",
        prompt.title
    ))
}

/// The open notes field of a question read by its answers' roles: Muse Code's note ([`is_muse_question`]), closed, and its text cleared, by Esc or Tab in it.
#[must_use]
pub fn roles_question_notes(
    prompt: &PromptV2,
    rows: &[String],
) -> Option<aterm_phase::muse::QuestionNotes> {
    if is_muse_question(prompt) {
        aterm_phase::muse::question_notes(rows)
    } else {
        None
    }
}

/// MUSE CODE'S REVIEW ([`aterm_phase::muse::is_review`], measured on
/// 1.4.10): a question answered `None of the above` with a note is shown
/// once more before it is sent, `⟩ Submit answers` focused over `Interrupt
/// turn`. Under a text word it is submitted — Enter on `Submit answers`,
/// guarded on the review's key row (the head's clock ticks) — only while it
/// holds ONE answer and that answer is exactly `<header>: None of the above;
/// note: <the word's text>`, the text the loop remembers writing (`own`), with
/// the cursor where Muse Code draws it.
fn review_answer(
    prompt: &PromptV2,
    rows: &[String],
    priority: QuestionPriority,
    own: Option<QuestionPriority>,
) -> Decision {
    let Some(text) = priority.text() else {
        return Decision::escalate(
            "a review of answers the supervisor did not give (its answers send no question to \
             one): a person's to submit",
        );
    };
    if own != Some(priority) {
        return Decision::escalate(begun("a review of a note the supervisor did not write"));
    }
    let header = prompt
        .title
        .strip_prefix(aterm_phase::anchor("muse.question.title"))
        .map(str::trim)
        .unwrap_or_default();
    if header.is_empty() || header.contains(", ") || prompt.head_off_screen {
        return Decision::escalate(begun("a review whose one question is not read"));
    }
    let want = format!(
        "{header}: {}{}{text}",
        aterm_phase::anchor("muse.question.none"),
        aterm_phase::anchor("muse.question.review.note")
    );
    if prompt.description != want {
        return Decision::escalate(begun(
            "its review holds other answers than the note the supervisor wrote",
        ));
    }
    let submit = aterm_phase::anchor("muse.question.submit");
    let (Some(at), Some(focus)) = (
        prompt.options.iter().position(|o| o.label == submit),
        prompt.options.iter().position(|o| o.focused),
    ) else {
        return Decision::escalate("a review with no `Submit answers` or no cursor");
    };
    if at != focus {
        return Decision::escalate(begun("the cursor was moved off `Submit answers`"));
    }
    let Some(guard) = prompt
        .question
        .and_then(|k| rows.get(k))
        .map(|r| row_guard(r))
    else {
        return Decision::escalate("a review whose key row is not on the rows read");
    };
    let word = priority.word();
    Decision::Approve {
        rule_id: priority.rule_id(),
        choice: Choice::Focus {
            steps: 0,
            label: submit.to_string(),
        },
        guard,
        subject: format!(
            "{} → {submit} (the {word} policy text as its note)",
            prompt.title
        ),
        unproven: Some(format!(
            "policy {word}: the worker decides: the review of its policy text written as the note \
             of `{}`",
            aterm_phase::anchor("muse.question.none")
        )),
    }
}

/// What a question read by its answers' roles is remembered by across reads (the loop's memory of the note it wrote, its fallback latch): its title, its question and its answers' labels.
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

/// The next keystroke of a note ([`note_step`]), with the guard of the row showing the state it answers ([`row_guard`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteStep {
    /// `down` (or `up`) toward `None of the above`, guarded on the focused answer's row.
    Move { down: bool, guard: String },
    /// `tab` on the focused `None of the above`: its notes field opens.
    Open { guard: String },
    /// The word's text, written into the open, empty notes field.
    Type { guard: String },
    /// `enter` on the notes field showing the word's text: the question answered `None of the above` with the text as its note.
    Enter { guard: String, whole: bool },
}

/// What a notes field shows of the policy text the loop wrote into it ([`note_read`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteRead {
    /// Exactly the text.
    Whole,
    /// Its tail from the start of a word, the rest of it above the rows the field draws: a field drawn fewer rows high than the text (a short pane) scrolled to its cursor shows the text's last rows.
    Tail,
    /// A piece of it that stops short of its end: a text still being drawn.
    Part,
    /// Nothing, or anything else.
    Other,
}

/// What the notes field drawing `shown` ([`aterm_phase::muse::QuestionNotes::text`]) shows of `text` ([`NoteRead`]).
#[must_use]
pub fn note_read(text: &str, shown: &str) -> NoteRead {
    if shown.is_empty() {
        NoteRead::Other
    } else if shown == text {
        NoteRead::Whole
    } else if text
        .strip_suffix(shown)
        .is_some_and(|head| head.ends_with(' '))
    {
        NoteRead::Tail
    } else if text.contains(shown) {
        NoteRead::Part
    } else {
        NoteRead::Other
    }
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

/// A note's next keystroke on a fresh read of a question read by its answers' roles (Muse Code's), under text priority `priority`, the loop remembering `own` written into it: with the notes field closed, the focus moved onto `None of the above`, then Tab; on the open empty field, the text; on the field drawing the text the loop wrote — all of it, or its tail where the field is too short for it ([`NoteRead::Tail`]) — Enter.
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
    match roles_question_notes(prompt, rows) {
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
        Some(n) if mine => match note_read(text, &n.text) {
            NoteRead::Whole => Ok(NoteStep::Enter {
                guard: guard_of(n.row)?,
                whole: true,
            }),
            NoteRead::Tail => Ok(NoteStep::Enter {
                guard: guard_of(n.row)?,
                whole: false,
            }),
            NoteRead::Part | NoteRead::Other => Err(begun(
                "text in its notes field the supervisor did not write",
            )),
        },
        Some(_) => Err(begun(
            "text in its notes field the supervisor did not write",
        )),
    }
}

/// A question read by its answers' roles (Muse Code's) under a text priority
/// (module header): the word's text as a note under `None of the above`
/// ([`Choice::Note`], carried out one keystroke at a time by the loop on
/// [`note_step`]).
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
    if !prompt.options.iter().any(|o| o.role == Role::Answer) {
        return Decision::escalate(NOT_READ_WHOLE);
    }
    let word = priority.word();
    let fallback = |why: &str| fall_back_to(answer_by_role(prompt, rows), priority, why);
    let open = roles_question_notes(prompt, rows).is_some();
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
        if !is_muse_question(prompt) {
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
    let Some(guard) = rows
        .get(prompt.question.unwrap_or(prompt.span.0))
        .map(|r| row_guard(r))
    else {
        return Decision::escalate("a question whose title row is not on the rows read");
    };
    Decision::Approve {
        rule_id: priority.rule_id(),
        choice: Choice::Note { text: priority },
        guard,
        subject: format!(
            "{} → {} (the {word} policy text as its note)",
            prompt.description,
            aterm_phase::anchor("muse.question.none")
        ),
        unproven: Some(format!(
            "policy {word}: the worker decides: its policy text written as the note of `{}`",
            aterm_phase::anchor("muse.question.none")
        )),
    }
}

/// Why a question read by its answers' roles is a person's when none of its
/// options was read as an answer ([`answer_by_role`], [`note_answer`]).
const NOT_READ_WHOLE: &str =
    "a question aterm could not read whole: no dialog, and no answer to choose";

/// A question read by its answers' roles, not as a dialog (agy's, Muse
/// Code's): the answer marked `(Recommended)`, else the first.
fn answer_by_role(prompt: &PromptV2, rows: &[String]) -> Decision {
    let answers: Vec<&aterm_phase::Opt> = prompt
        .options
        .iter()
        .filter(|o| o.role == Role::Answer)
        .collect();
    let Some(&first) = answers.first() else {
        return Decision::escalate(NOT_READ_WHOLE);
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
    let subject = format!("{} → {}", prompt.title, o.label);
    if prompt.select == Select::ArrowsEnter {
        let (Some(from), Some(to)) = (
            prompt.options.iter().position(|q| q.focused),
            prompt.options.iter().position(|q| std::ptr::eq(q, o)),
        ) else {
            return Decision::escalate("a question whose cursor is on no option");
        };
        let Some(guard) = prompt
            .question
            .or(Some(prompt.span.0))
            .and_then(|k| rows.get(k))
            .map(|r| row_guard(r))
        else {
            return Decision::escalate("a question whose question row is not on the rows read");
        };
        return Decision::Approve {
            rule_id: RULE_ANSWER_RECOMMENDED,
            choice: Choice::Focus {
                steps: i32::try_from(to).unwrap_or(0) - i32::try_from(from).unwrap_or(0),
                label: o.label.clone(),
            },
            guard,
            subject,
            unproven: Some(how),
        };
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
        subject,
        unproven: Some(how),
    }
}

/// A person has begun answering: theirs to finish.
pub(crate) fn begun(what: &str) -> String {
    format!("a question a person has begun answering ({what}): theirs to finish")
}

/// Claude Code's SECURITY-ALERT model switch, and the option that stays put.
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
    if destructive(question, options).is_some() {
        return None;
    }
    let stay = options.iter().find(|o| {
        let label = o.label.trim().to_lowercase();
        label.starts_with("stay on ")
            && o.description.to_lowercase().contains("without switching")
            && !aterm_phase::prompt::settles_later(&format!("{}\n{}", o.label, o.description))
    })?;
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
