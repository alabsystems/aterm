// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE QUESTION ANSWER: which key the supervisor presses on an agent's
//! question dialog — Claude Code's AskUserQuestion as aterm-phase reads it
//! whole ([`aterm_phase::QuestionDialog`]), and Codex's `Question 1/1` by its
//! answers' roles — [`answer_question`], ONE rule
//! [`RULE_ANSWER_RECOMMENDED`], under `[harness] answer_questions`
//! ([`super::approval::ApprovalCtx::answer_questions`], on by default,
//! independent of `approve`: a question is no permission).
//!
//! **The ruling it implements** (owner directive of 2026-09-25): "the
//! harness must not stall on escalating to the user. If this is a Claude
//! Code setting, set it so these prompts don't happen; otherwise the harness
//! must choose the recommended option(s) and continue automatically." No
//! Claude Code setting removes the dialog while leaving the model able to
//! ask (measured on 2.1.282: the design of 2026-09-25, §A), so the harness
//! answers it: every option whose LABEL is marked `(Recommended)` — the
//! tool's own convention, read from the label only, never a description
//! ([`aterm_phase::Recommended`]) — or option 1 when none is.
//!
//! **Which option.** Single-select and the preview form: the lowest-numbered
//! `Label` option, else the lowest `Wrapped` one (a label whose last row
//! may be its wrap ends `(Recommended)`, [`aterm_phase::Recommended::Wrapped`]:
//! pressed, and the journal says so), else option 1. Multi-select: every
//! `Label` option, else every `Wrapped` one, else option 1 — each toggled
//! in turn, then the button (`Next`, or `Submit` on the last question). The
//! review (Submit) tab: `1. Submit answers`. NEVER the free-text row (`Type
//! something.`, read by its position), the chat row (`Chat about this`),
//! the review's cancel, Esc or `n` — [`AnswerTarget`] cannot name them.
//!
//! **How: Enter on the focused row, never a digit** ([`Answer::FocusEnter`];
//! the critique of 2026-09-25, R4): the focus is moved to the target one row
//! at a time, each move seen landing on a fresh read, and Enter is pressed
//! guarded on the focused row as drawn (`❯ 1. Outlined on meters
//! (Recommended)`, `❯ 3. [ ] Emoji`) under the screen-generation fence — the
//! loop's part (`Session::press_question`). The vendor opens every tab with
//! the focus on option 1 and the review on `1. Submit answers`, and the tool
//! asks the model to put its recommendation first, so the common answer is
//! one Enter with no move.
//!
//! **Codex's question** (measured on 0.156.1) is no dialog read whole: its
//! reader gives each of the model's answers [`Role::Answer`] (its `None of
//! the above` row [`Role::Other`]), and the answer is the one marked
//! `(Recommended)` ([`PromptV2::recommended`]), else the first — by its
//! DIGIT, which Codex takes at once (measured), guarded on the dialog's
//! title row ([`answer_by_role`]). Its Esc interrupts the whole turn and is
//! never pressed.
//!
//! **A person's answer is theirs** (the critique's R2, person evidence):
//!
//! * (c) a preview option drawn as selected (` ✔`: a tab a person answered
//!   and came back to), and (d) text in the free-text row or — multi-select
//!   — a checked option outside the recommended set, or a checked free-text
//!   row: ESCALATED, never overwritten. A check INSIDE the recommended set
//!   that the supervisor did not make, and one it made that a person took
//!   off, look like its own progress here; the loop, which remembers its
//!   toggles, hands those over (`approval_loop.rs`).
//! * (a) the focus where neither the vendor nor the harness put it, and (b)
//!   the review's `⚠ You have not answered all questions` (only a person's
//!   Tab or `→` skips a question): answered, but only once the person has
//!   been quiet — the loop keys no question while the session's person stamp
//!   (`text --json`'s `"human_ms"`) is younger than `[harness]
//!   human_grace_s`, for every question key, not only these. The unanswered
//!   review is submitted (the person's skip stands) and the journal names
//!   the headers skipped.
//!
//! **What is never answered, and escalated** (`a question (AskUserQuestion)
//! aterm-phase did not read whole: <why>` and its siblings): a form aterm-phase
//! did not read whole ([`QuestionForm::Unknown`] — a column-1 shape 2.1.282
//! does not draw, a scrolled list, an opened notes field, a dialog withheld
//! behind a draft in the composer, a numbering gap); no visible focus (R11:
//! 2.1.282 always draws one); a question with NO recommended option whose
//! question, labels or descriptions name a destructive act
//! ([`super::turn_end::DESTRUCTIVE_WORDS`], R10: option 1 of "Drop the
//! table?" would be `Yes`) — unless one option, and one only, REFUSES it
//! ([`refuses`]: `No`, `No, keep it`) and names no act itself: that option
//! is answered (D2 of the reconciliation of 2026-09-25 — nobody is there to
//! ask, and the refusal consents to nothing); a multi-select's is never
//! toggled; and a focus more than
//! [`MAX_QUESTION_FOCUS_STEPS`] rows from the target. A MARKED recommendation
//! is answered whatever it says: it is the model's own labelled option, and
//! what it leads to arrives as its own permission box.
//!
//! Pure: the reading and the rows in, a [`Decision`] out. The loop adds what
//! it alone can know — the person's quiet, the key in flight, the retries
//! (`approval_loop.rs`).

use aterm_phase::prompt::{PromptV2, Role};
use aterm_phase::{
    QuestionDialog, QuestionFocus, QuestionForm, QuestionOption, QuestionText, Recommended,
};

use super::approval::{Answer, AnswerTarget, Choice, Decision};
use super::guard::row_guard;

/// The rule id of an answered question (the ledger's, the journal's).
pub const RULE_ANSWER_RECOMMENDED: &str = "answer-recommended@v1";

/// The most rows the focus is moved before the Enter: the furthest measured
/// move is option 1 to `Next` in a four-option multi-select, five.
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

/// Where `focus` stands in the dialog's FOCUS ORDER, from 0: the options,
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
    }
}

/// The destructive word a question with no recommended option names in its
/// question, its labels or its descriptions (R10), lowercased like the
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

/// Whether an option's label REFUSES what its question asks (D2): `No`
/// itself, or `No` then a separator (`No, keep it`, `No — leave it`, `No
/// (skip)`) — never a word that starts with it (`None of these`, `Not
/// now`, `Now`).
fn refuses(label: &str) -> bool {
    let l = label.trim().to_lowercase();
    l.strip_prefix("no")
        .is_some_and(|rest| rest.chars().next().is_none_or(|c| !c.is_alphanumeric()))
}

/// The ONE option that refuses a question that would `w`, naming no act
/// itself — or why none can answer it (D2).
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

/// The journal's `unproven` for an answer by the refusing option (D2).
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

/// THE DECISION on one question dialog (module header). `prompt` is a box of
/// kind [`aterm_phase::PromptKind::Question`], `rows` the screen it was read
/// from: the press is guarded on the question's first row as drawn (the
/// review's `1. Submit answers` row), and the loop guards its Enter on the
/// focused row as drawn.
#[must_use]
pub fn answer_question(prompt: &PromptV2, rows: &[String]) -> Decision {
    let Some(dialog) = prompt.question_dialog.as_ref() else {
        return answer_by_role(prompt, rows);
    };
    if let QuestionForm::Unknown { why } = dialog.form {
        return Decision::escalate(format!(
            "a question (AskUserQuestion) aterm-phase did not read whole: {why}"
        ));
    }
    let focus = dialog.focus();
    let Some(from) = focus_position(dialog, focus) else {
        return Decision::escalate(
            "a question with no visible focus: aterm-phase did not read where the ❯ is",
        );
    };
    let Some(guard) = dialog
        .guard_row()
        .and_then(|k| rows.get(k))
        .map(|r| row_guard(r))
    else {
        return Decision::escalate("a question whose question row is not on the rows read");
    };
    let (target, subject, unproven) = match choose(dialog) {
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
        Some(note) if steps != 0 => format!("{unproven}; {note}"),
        _ => unproven,
    };
    Decision::Approve {
        rule_id: RULE_ANSWER_RECOMMENDED,
        choice: Choice::Answer(Answer::FocusEnter { steps, target }),
        guard,
        subject,
        unproven: Some(unproven),
    }
}

/// A question read by its answers' roles, not as a dialog (module header,
/// "Codex's question"): the answer marked `(Recommended)`, else the first,
/// by its digit, guarded on the dialog's title row. An unmarked one that
/// names a destructive act (R10) is answered by its one refusal, as a
/// dialog's is (D2), and escalated when it has none. A question whose head
/// is off the screen, or with no answer to choose, is escalated.
fn answer_by_role(prompt: &PromptV2, rows: &[String]) -> Decision {
    let answers: Vec<&aterm_phase::Opt> = prompt
        .options
        .iter()
        .filter(|o| o.role == Role::Answer)
        .collect();
    let Some(&first) = answers.first() else {
        return Decision::escalate(
            "a question aterm-phase did not read whole: no dialog, and no answer to choose",
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

/// A person has begun answering: theirs to finish (R2 (d)).
pub(crate) fn begun(what: &str) -> String {
    format!("a question a person has begun answering ({what}): theirs to finish")
}

/// The row to press Enter on, what the ledger names, and the journal's
/// `unproven` — or why the question is the person's.
fn choose(dialog: &QuestionDialog) -> Result<(AnswerTarget, String, String), String> {
    let label_of = |options: &[QuestionOption], n: u8| {
        options
            .iter()
            .find(|o| o.n == n)
            .map(|o| o.label.clone())
            .unwrap_or_default()
    };
    let no_mark_that_would = |question: &QuestionText, options: &[QuestionOption]| {
        destructive(question, options).map(|w| {
            format!("a question with no recommended option that would {w}: a person answers that")
        })
    };
    // D2: an unmarked question that would do something destructive, answered
    // by its one refusal — the option, what the ledger names, `unproven`.
    let refusal = |question: &QuestionText, options: &[QuestionOption]| {
        destructive(question, options).map(|w| {
            the_refusal(w, options, |o| format!("{}\n{}", o.label, o.description)).map(|o| {
                (
                    AnswerTarget::Option(o.n),
                    format!("{} → {}", question.text, o.label),
                    refused(w),
                )
            })
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
            let (marked, how) = recommended(options);
            if how == Chosen::Unmarked
                && let Some(answer) = refusal(question, options)
            {
                return answer;
            }
            let n = marked[0];
            Ok((
                AnswerTarget::Option(n),
                format!("{} → {}", question.text, label_of(options, n)),
                how.unproven(),
            ))
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
            let (marked, how) = recommended(options);
            if how == Chosen::Unmarked
                && let Some(answer) = refusal(question, options)
            {
                return answer;
            }
            let n = marked[0];
            Ok((
                AnswerTarget::Option(n),
                format!("{} → {}", question.text, label_of(options, n)),
                how.unproven(),
            ))
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
            let open: Vec<&str> = dialog
                .tabs
                .iter()
                .filter(|t| !t.answered)
                .map(|t| t.header.as_str())
                .collect();
            if *unanswered || !open.is_empty() {
                let open = if open.is_empty() {
                    "the headers not read".to_string()
                } else {
                    open.join(", ")
                };
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
        QuestionForm::Unknown { why } => Err(format!(
            "a question (AskUserQuestion) aterm-phase did not read whole: {why}"
        )),
    }
}

#[cfg(test)]
#[path = "question_tests.rs"]
mod tests;
