// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm drive answer @<worker> [--box TOKEN] <answer>`: a controlling session
//! types its own choice into a worker's question — Claude Code's question
//! dialog (AskUserQuestion), or agy's and Muse Code's. The worker's window
//! hands the question over when the session's word is `ask` (`aterm ctl
//! @<worker> meta set questions ask`); the controller reads it (`aterm drive
//! phase`) and answers it with this. The question is read by the worker's own
//! program's reader ([`Session::program_reader`]), as the window reads it.
//!
//! One screen at a time, through the window supervisor's own keys. On Claude
//! Code's dialog ([`Session::press_question`]): the focus moved one row per
//! key, each move seen landing, then Enter guarded on the chosen row under the
//! screen-generation fence. Never a digit, Esc, `n`, or a key on the free-text
//! row, chat row or review's cancel; a person keying meanwhile gets it
//! (`LEFT`). The answer:
//!
//! * `<n>` or an option's exact label (case folded): that option;
//! * on a multi-select, the options to check, `, `-joined, then the button; refused while the
//!   free-text row holds text;
//! * `submit` on the review tab;
//! * `recommended`: what the window presses under the `recommended` priority
//!   ([`super::super::policy::question::answer_question`]), the whole tab on a multi-select;
//! * `human`: nothing typed, `LEFT`.
//!
//! On a question read by its ANSWERS' ROLES (agy's, Muse Code's: no
//! dialog read whole), the window's own key for it: the answer's digit, which
//! agy chooses at once ([`Session::press_one_guarded`]), guarded on
//! the question's title row; on Muse Code's, which its reader says is chosen
//! by the cursor and Enter (a digit there only moves the cursor, measured),
//! the focus moved to the answer, each move seen landing, then Enter guarded
//! on the answer's own row ([`Session::press_focused`]), every key fenced on
//! the person count of the read. Each fenced on the screen generation —
//! never unfenced (a host that cannot fence it, or turns the fenced key away,
//! is NOT-SERVED with nothing typed), and never Esc or the row that asks for
//! the person's own words (`None of the above`). The answer is `<n>` or an
//! answer's exact label (case folded), `recommended` (the window's own: the
//! `(Recommended)` answer, else the first) or `human`; a person who keyed
//! within the grace gets it (`LEFT`).
//!
//! Anything else is refused: free text is a person's to type. `long-term` and
//! `short-term`, where they name no option, are refused with the `meta set
//! questions <word>` that has the window answer by that policy (the one writer
//! of its text, which remembers writing it). `--box TOKEN` refuses a question
//! whose token ([`prompt_box_token`]) differs, so an answer never lands on
//! another question.
//!
//! Prints one line: `ANSWERED @<w> enters=<k> answer=<what> next=<…>` (exit
//! 0; `enters=` counts the keys that answered: the dialog's Enters, agy's
//! one digit, Muse Code's one Enter), `LEFT @<w> <why>` (exit 0), `NO-BOX @<w> …` (exit 1), `REFUSED
//! @<w> <why>` (exit 2, nothing typed), `NOT-SERVED @<w> <why> (enters=<k>)`
//! (exit 3: the host or the question did not take the answer as decided).

use std::io::Write;

use aterm_phase::{Opt, QuestionDialog, QuestionForm, QuestionOption, Role};

use super::super::policy::approval::{Answer, AnswerTarget, Choice, Decision, QuestionPriority};
use super::super::policy::question::{
    Pane, answer_question, focus_position, recommended_set, target_focus,
};
use super::super::policy::row_guard;
use super::*;

/// No question dialog on the screen.
pub const EXIT_NO_BOX: u8 = 1;
/// The answer does not fit the dialog, or `--box` names another: nothing typed.
pub const EXIT_REFUSED: u8 = 2;
/// A key the host or the dialog did not take as decided.
pub const EXIT_NOT_SERVED: u8 = 3;

/// The dialog's TOKEN: eight hex digits over its form, its tab headers and
/// the question on display (the review: its headers) — the same on every
/// read of that screen, another for another question.
#[must_use]
pub fn question_box_token(dialog: &QuestionDialog) -> String {
    let mut name = String::new();
    let (form, question) = match &dialog.form {
        QuestionForm::Single { question, .. } => ("single", question.text.as_str()),
        QuestionForm::Multi { question, .. } => ("multi", question.text.as_str()),
        QuestionForm::Preview { question, .. } => ("preview", question.text.as_str()),
        QuestionForm::Review { .. } => ("review", ""),
        QuestionForm::Unknown { .. } => ("unknown", ""),
    };
    name.push_str(form);
    for tab in &dialog.tabs {
        name.push('\u{1f}');
        name.push_str(&tab.header);
    }
    name.push('\u{1e}');
    name.push_str(question);
    token_of(&name)
}

/// The TOKEN of a question read by its answers' roles (agy's, Muse Code's: no dialog
/// read whole): eight hex digits over its title, its question and its
/// options' labels — the same on every read of that screen, another for
/// another question.
fn role_box_token(prompt: &aterm_phase::PromptV2) -> String {
    let mut name = format!("roles\u{1f}{}\u{1e}{}", prompt.title, prompt.description);
    for o in &prompt.options {
        name.push('\u{1f}');
        name.push_str(&o.label);
    }
    token_of(&name)
}

/// The token `phase` prints as `box` and `--box` names: a dialog's
/// ([`question_box_token`]), else, for a question read by its answers' roles,
/// [`role_box_token`]; `None` for any other box (a question with no answer
/// read has nothing to answer).
#[must_use]
pub fn prompt_box_token(prompt: &aterm_phase::PromptV2) -> Option<String> {
    match &prompt.question_dialog {
        Some(dialog) => Some(question_box_token(dialog)),
        None => (prompt.kind == PromptKind::Question
            && prompt.options.iter().any(|o| o.role == Role::Answer))
        .then(|| role_box_token(prompt)),
    }
}

/// Eight hex digits over `name`.
fn token_of(name: &str) -> String {
    let h = aterm_hash::fnv1a_64(name.as_bytes());
    format!("{:08x}", (h ^ (h >> 32)) & 0xffff_ffff)
}

/// What one screen's answer is, planned against the question as read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    /// Nothing typed: the person answers.
    Human,
    /// Enter on each target in turn (a multi-select's toggles, then its
    /// button), re-read and re-planned between them; `said` names it.
    Keys {
        targets: Vec<AnswerTarget>,
        said: String,
    },
    /// The digit of a question read by its answers' roles.
    Digit(Digit),
    /// The focus moved to an answer of a question read by its answers'
    /// roles that is chosen by the cursor and Enter (Muse Code's), then
    /// Enter on it.
    Focus(Focus),
}

/// An answer's digit on a question read by its answers' roles (agy's),
/// which chooses at once: `n`, guarded on the question's title row
/// (`guard`); `said` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Digit {
    n: u8,
    guard: String,
    said: String,
}

/// An answer of a question read by its answers' roles that is chosen by the
/// cursor and Enter (Muse Code's): the focus moved `steps` options to the one
/// labelled `label`, the moves guarded on the question's own row (`guard`),
/// then Enter on it; `said` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Focus {
    steps: i32,
    label: String,
    guard: String,
    said: String,
}

/// The option an answer word names on `options`: its number, or its label
/// (trimmed, case folded).
fn option_named<'a>(options: &'a [QuestionOption], word: &str) -> Option<&'a QuestionOption> {
    let word = word.trim();
    if let Ok(n) = word.parse::<u8>() {
        return options.iter().find(|o| o.n == n);
    }
    options
        .iter()
        .find(|o| o.label.trim().eq_ignore_ascii_case(word))
}

/// Why `answer` is refused as the text priority `p` (module header): the
/// window's policy, never an answer typed from here. Its text is written by
/// the loop that remembers writing it, and an out-of-process write would be a
/// second owner of the free-text row; the session's word asks the window to.
fn policy_refusal(answer: &str, who: &str, p: QuestionPriority) -> String {
    format!(
        "{answer:?} is a question policy, not an answer: `aterm ctl {who} meta set questions \
         {}` has the window answer this session's questions by it (an answer here is an \
         option, `submit`, `recommended` or `human`)",
        p.word()
    )
}

/// Why `answer`, which names no option, is refused: a text priority's
/// [`policy_refusal`], else free text, a person's to type.
fn names_no_option(answer: &str, who: &str) -> String {
    match QuestionPriority::parse(answer).filter(|p| p.text().is_some()) {
        Some(p) => policy_refusal(answer, who, p),
        None => format!("{answer:?} names no option: a free-text answer is a person's to type"),
    }
}

/// Plan `answer` on the question on display (module header), for the worker
/// `who`: a dialog read whole, else by its answers' roles ([`plan_by_role`]).
/// `Err` is the refusal: nothing is typed.
fn plan(
    prompt: &aterm_phase::PromptV2,
    rows: &[String],
    answer: &str,
    who: &str,
) -> Result<Plan, String> {
    let Some(dialog) = prompt.question_dialog.as_ref() else {
        return plan_by_role(prompt, rows, answer, who);
    };
    let answer = answer.trim();
    if answer.eq_ignore_ascii_case("human") {
        return Ok(Plan::Human);
    }
    if answer.eq_ignore_ascii_case("recommended") {
        let decision = answer_question(
            prompt,
            rows,
            QuestionPriority::Recommended,
            None,
            false,
            Pane::of(rows),
        );
        return match decision {
            // A multi-select's recommended answer is the whole tab, but the decider returns
            // one step of it: once it approves, plan the explicit `a, b` answer (each unchecked
            // recommended option toggled, then the button). Nothing is ever unchecked.
            Decision::Approve {
                choice: Choice::Answer(answer),
                ..
            } if matches!(answer.enter_target(), Some(AnswerTarget::Option(_)))
                && matches!(dialog.form, QuestionForm::Multi { .. }) =>
            {
                let QuestionForm::Multi {
                    question,
                    options,
                    button,
                    ..
                } = &dialog.form
                else {
                    return Err("the dialog is no multi-select".to_string());
                };
                let want = recommended_set(options);
                let mut targets: Vec<AnswerTarget> = options
                    .iter()
                    .filter(|o| want.contains(&o.n) && o.checked != Some(true))
                    .map(|o| AnswerTarget::Option(o.n))
                    .collect();
                targets.push(AnswerTarget::Button);
                let labels = options
                    .iter()
                    .filter(|o| want.contains(&o.n))
                    .map(|o| o.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                Ok(Plan::Keys {
                    targets,
                    said: format!("{} → {} ({labels})", question.text, button.label),
                })
            }
            Decision::Approve {
                choice: Choice::Answer(answer),
                subject,
                ..
            } if answer.enter_target().is_some() => Ok(Plan::Keys {
                targets: answer.enter_target().into_iter().collect(),
                said: subject,
            }),
            Decision::Approve { .. } | Decision::Decline { .. } => {
                Err("the window's own answer is no question key".to_string())
            }
            Decision::Escalate { reason } => Err(format!(
                "the window would not answer this dialog itself: {reason}"
            )),
        };
    }
    // A text priority is refused where it names no option on the tab
    // ([`policy_refusal`]).
    let policy = QuestionPriority::parse(answer).filter(|p| p.text().is_some());
    let free_text = || names_no_option(answer, who);
    match &dialog.form {
        QuestionForm::Single { options, .. } | QuestionForm::Preview { options, .. } => {
            let o = option_named(options, answer).ok_or_else(free_text)?;
            Ok(Plan::Keys {
                targets: vec![AnswerTarget::Option(o.n)],
                said: o.label.clone(),
            })
        }
        // The button submits the free-text row with the checks when the row
        // holds text (ticked): an answer the controller did not name, and a
        // free-text answer — never sent from here. Refused before any key.
        QuestionForm::Multi { free_text: row, .. }
            if !row.pristine || row.checked == Some(true) =>
        {
            Err(
                "the free-text row holds text, which the button would submit with the options \
                 named: a free-text answer is a person's to finish (`human` leaves the dialog)"
                    .to_string(),
            )
        }
        QuestionForm::Multi { options, .. } => {
            let mut want = Vec::new();
            for word in answer.split(',').filter(|w| !w.trim().is_empty()) {
                let o = option_named(options, word).ok_or_else(free_text)?;
                if !want.contains(&o.n) {
                    want.push(o.n);
                }
            }
            if want.is_empty() {
                return Err(free_text());
            }
            let mut targets: Vec<AnswerTarget> = options
                .iter()
                .filter(|o| o.checked.unwrap_or(false) != want.contains(&o.n))
                .map(|o| AnswerTarget::Option(o.n))
                .collect();
            targets.push(AnswerTarget::Button);
            let said = options
                .iter()
                .filter(|o| want.contains(&o.n))
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Ok(Plan::Keys { targets, said })
        }
        QuestionForm::Review { .. } if answer.eq_ignore_ascii_case("submit") => Ok(Plan::Keys {
            targets: vec![AnswerTarget::ReviewSubmit],
            said: "submit".to_string(),
        }),
        QuestionForm::Review { .. } => Err(match policy {
            Some(p) => policy_refusal(answer, who, p),
            None => format!(
                "{answer:?} on the review tab: `submit` sends the answers shown, `human` leaves it"
            ),
        }),
        QuestionForm::Unknown { why } => Err(format!("the dialog was not read whole: {why}")),
    }
}

/// Plan `answer` on a question read by its ANSWERS' ROLES (agy's and Muse
/// Code's, which no dialog reader reads whole), as the window answers one: by
/// the answer's digit, guarded on the question's title row. `recommended` is
/// the window's own pick under the `recommended` priority (its
/// `(Recommended)` answer, else the first; an unmarked destructive question
/// its one refusal), refused where the window would hand it over. An option
/// that is no answer (the last row, `None of the above`) asks for the
/// person's own words, and is refused as free text. A question its reader
/// says is chosen by the cursor and Enter (Muse Code's, measured on 1.4.10:
/// no digit was tried on it) is planned as the focus moved to the answer
/// ([`Plan::Focus`]), guarded on the question's own row, never its head,
/// whose clock ticks. `Err` is the refusal: nothing is typed.
fn plan_by_role(
    prompt: &aterm_phase::PromptV2,
    rows: &[String],
    answer: &str,
    who: &str,
) -> Result<Plan, String> {
    let answer = answer.trim();
    if answer.eq_ignore_ascii_case("human") {
        return Ok(Plan::Human);
    }
    if !prompt.options.iter().any(|o| o.role == Role::Answer) {
        return Err("the dialog was not read whole".to_string());
    }
    if answer.eq_ignore_ascii_case("recommended") {
        let decision = answer_question(
            prompt,
            rows,
            QuestionPriority::Recommended,
            None,
            false,
            Pane::of(rows),
        );
        return match decision {
            Decision::Approve {
                choice: Choice::Digit(n),
                guard,
                subject,
                ..
            } => Ok(Plan::Digit(Digit {
                n,
                guard,
                said: subject,
            })),
            Decision::Approve {
                choice: Choice::Focus { steps, label },
                guard,
                subject,
                ..
            } => Ok(Plan::Focus(Focus {
                steps,
                label,
                guard,
                said: subject,
            })),
            Decision::Approve { .. } | Decision::Decline { .. } => {
                Err("the window's own answer is no question key".to_string())
            }
            Decision::Escalate { reason } => Err(format!(
                "the window would not answer this dialog itself: {reason}"
            )),
        };
    }
    let named = |o: &&Opt| match answer.parse::<u8>() {
        Ok(n) => o.n == Some(n),
        Err(_) => o.label.trim().eq_ignore_ascii_case(answer),
    };
    let Some(o) = prompt.options.iter().find(named) else {
        return Err(names_no_option(answer, who));
    };
    let Some(n) = o.n.filter(|_| o.role == Role::Answer) else {
        return Err(format!(
            "{:?} asks for the person's own words: a free-text answer is a person's to type",
            o.label
        ));
    };
    // The guard binds the digit to this question: a head off the rows read
    // has no title row to bind it to.
    if prompt.head_off_screen {
        return Err(format!(
            "the question's head is off the screen: {}",
            prompt.title
        ));
    }
    if prompt.select == aterm_phase::prompt::Select::ArrowsEnter {
        let (Some(from), Some(to)) = (
            prompt.options.iter().position(|q| q.focused),
            prompt.options.iter().position(|q| std::ptr::eq(q, o)),
        ) else {
            return Err("the question's cursor is on no option".to_string());
        };
        let Some(guard) = prompt
            .question
            .and_then(|k| rows.get(k))
            .map(|r| row_guard(r))
        else {
            return Err("the question's own row is not on the rows read".to_string());
        };
        return Ok(Plan::Focus(Focus {
            steps: i32::try_from(to).unwrap_or(0) - i32::try_from(from).unwrap_or(0),
            label: o.label.clone(),
            guard,
            said: o.label.clone(),
        }));
    }
    let Some(guard) = rows.get(prompt.span.0).map(|r| row_guard(r)) else {
        return Err("the question's title row is not on the rows read".to_string());
    };
    Ok(Plan::Digit(Digit {
        n,
        guard,
        said: o.label.clone(),
    }))
}

/// The question on display, read by `reader`, for `next=`: its text,
/// `review`, or `none`.
fn next_of(reader: &dyn ScreenReader, rows: &[String]) -> String {
    let Some(p) = reader.prompt(rows) else {
        return "none".to_string();
    };
    match p.question_dialog {
        Some(d) => match (&d.form, d.question()) {
            (QuestionForm::Review { .. }, _) => "review".to_string(),
            (_, Some(q)) => q.text.clone(),
            _ => "question".to_string(),
        },
        None if p.kind == PromptKind::Question && !p.description.is_empty() => p.description,
        None if p.kind == PromptKind::Question => "question".to_string(),
        None => "none".to_string(),
    }
}

/// `aterm drive answer`'s options.
#[derive(Debug, Clone)]
pub struct AnswerOpts {
    /// The controller's answer (module header).
    pub answer: String,
    /// `--box`: the question's token the answer was decided on.
    pub box_token: Option<String>,
    /// A person's quiet before a key goes (`[harness] human_grace_s`): one
    /// who keyed within it gets the question (`LEFT`).
    pub grace: Duration,
}

impl<C: Ctl> Session<'_, C> {
    /// Answer the question on display (module header): one line to `out`,
    /// the exit code back.
    pub fn answer(&mut self, opts: &AnswerOpts, out: &mut dyn Write) -> Result<u8, String> {
        let who = self
            .setup
            .sid
            .clone()
            .unwrap_or_else(|| "@self".to_string());
        let who = if who.starts_with('@') {
            who
        } else {
            format!("@{who}")
        };
        let mut say = |line: String, code: u8| -> Result<u8, String> {
            writeln!(out, "{line}").map_err(|e| format!("cannot write stdout: {e}"))?;
            Ok(code)
        };
        let mut screen = self.screen().map_err(fail_text)?;
        // The worker's own program reads the question, as its window does:
        // Claude Code's dialog, or agy's and Muse Code's by their answers' roles.
        let reader = self.program_reader(&screen.rows)?;
        let Some(prompt) = reader
            .prompt(&screen.rows)
            .filter(|p| p.kind == PromptKind::Question)
        else {
            return say(
                format!("NO-BOX {who} no question dialog on the screen"),
                EXIT_NO_BOX,
            );
        };
        if let (Some(want), Some(on)) = (&opts.box_token, prompt_box_token(&prompt))
            && !on.eq_ignore_ascii_case(want.trim())
        {
            return say(
                format!(
                    "REFUSED {who} the dialog on the screen is box={on}, not the box={} the \
                     answer was for: nothing typed",
                    want.trim()
                ),
                EXIT_REFUSED,
            );
        }
        let (targets, said) = match plan(&prompt, &screen.rows, &opts.answer, &who) {
            Ok(Plan::Human) => return say(format!("LEFT {who} human: nothing typed"), 0),
            Ok(Plan::Keys { targets, said }) => (targets, said),
            Ok(Plan::Digit(digit)) => {
                let (line, code) = self.answer_digit(&who, reader, &screen, &digit, opts.grace)?;
                return say(line, code);
            }
            Ok(Plan::Focus(focus)) => {
                let (line, code) =
                    self.answer_focus(&who, reader, &prompt, &screen, &focus, opts)?;
                return say(line, code);
            }
            Err(why) => return say(format!("REFUSED {who} {why}"), EXIT_REFUSED),
        };
        let mut keys = 0usize;
        for target in targets {
            let Some(dialog) =
                aterm_phase::parse_prompt_v2(&screen.rows).and_then(|p| p.question_dialog)
            else {
                return say(
                    format!(
                        "NOT-SERVED {who} the dialog left before the answer was done (enters={keys})"
                    ),
                    EXIT_NOT_SERVED,
                );
            };
            let (Some(from), Some(to)) = (
                focus_position(&dialog, dialog.focus()),
                focus_position(&dialog, target_focus(target)),
            ) else {
                return say(
                    format!("NOT-SERVED {who} the dialog shows no focus to move (enters={keys})"),
                    EXIT_NOT_SERVED,
                );
            };
            let answer = Answer::FocusEnter {
                steps: to - from,
                target,
            };
            let why = match self
                .press_question(&answer, &screen, opts.grace)
                .map_err(fail_text)?
            {
                Pressing::Done(Press::Pressed { seq }) => {
                    keys += 1;
                    let _ = self.wait(&["seq", &seq.to_string()], STRAY_SETTLE);
                    let _ = self.wait(&["idle", SETTLE_MS], SETTLE_CAP);
                    screen = self.screen().map_err(fail_text)?;
                    continue;
                }
                Pressing::Done(Press::Yielded { .. }) => {
                    return say(
                        format!(
                            "LEFT {who} a person gave the session input: the dialog is theirs \
                             (enters={keys})"
                        ),
                        0,
                    );
                }
                Pressing::Done(Press::Changed { .. }) => {
                    "the dialog changed under the key (a person, or the worker)".to_string()
                }
                Pressing::Done(Press::Skipped { .. }) => {
                    "the guarded row was not on the screen when the key went".to_string()
                }
                Pressing::Done(Press::Unseen { .. }) => {
                    "a focus move's effect was not seen: nothing more was sent".to_string()
                }
                // `plan` never types a policy text: a controller's answer is
                // an option.
                Pressing::Done(Press::Typed { .. }) => {
                    "a policy text was written where an option was chosen".to_string()
                }
                Pressing::Halted { why }
                | Pressing::Refused { why }
                | Pressing::Unconfirmed { why, .. }
                | Pressing::Lost { why, .. } => why,
            };
            return say(
                format!("NOT-SERVED {who} {why} (enters={keys})"),
                EXIT_NOT_SERVED,
            );
        }
        say(
            format!(
                "ANSWERED {who} enters={keys} answer={said} next={}",
                next_of(reader, &screen.rows)
            ),
            0,
        )
    }

    /// Press `digit` on the question read by its answers' roles on `screen`
    /// (module header): the window's own key for it
    /// ([`Self::press_one_guarded`]), guarded on the question's title row and
    /// fenced on the screen generation and person count (never pressed
    /// without them), and only while a person has been quiet for `grace` by
    /// the read's stamp. The line and the exit code back.
    fn answer_digit(
        &mut self,
        who: &str,
        reader: &dyn ScreenReader,
        screen: &Screen,
        digit: &Digit,
        grace: Duration,
    ) -> Result<(String, u8), String> {
        if let HumanInput::Ago(ms) = screen.human
            && u128::from(ms) < grace.as_millis()
        {
            return Ok((
                format!(
                    "LEFT {who} a person gave the session input: the question is theirs \
                     (enters=0)"
                ),
                0,
            ));
        }
        // The guard alone can recur on a later question. The shared
        // digit path requires both stamps from this judged read and never
        // retries by dropping either one.
        let why = match self
            .press_one_guarded(
                &digit.guard,
                &digit.n.to_string(),
                screen,
                super::super::policy::KeyAct::Answer,
            )
            .map_err(fail_text)?
        {
            Pressing::Done(Press::Pressed { seq }) => {
                let _ = self.wait(&["seq", &seq.to_string()], STRAY_SETTLE);
                let _ = self.wait(&["idle", SETTLE_MS], SETTLE_CAP);
                let now = self.screen().map_err(fail_text)?;
                return Ok((
                    format!(
                        "ANSWERED {who} enters=1 answer={} next={}",
                        digit.said,
                        next_of(reader, &now.rows)
                    ),
                    0,
                ));
            }
            Pressing::Done(Press::Changed { .. }) => {
                "the question changed under the key (a person, or the worker)".to_string()
            }
            Pressing::Done(Press::Skipped { .. }) => {
                "the guarded row was not on the screen when the key went".to_string()
            }
            Pressing::Done(Press::Yielded { .. }) => {
                return Ok((
                    format!(
                        "LEFT {who} a person gave the session input: the question is theirs (enters=0)"
                    ),
                    0,
                ));
            }
            // A digit press moves no focus and writes no text.
            Pressing::Done(Press::Unseen { .. } | Press::Typed { .. }) => {
                "the key was not seen through".to_string()
            }
            Pressing::Halted { why }
            | Pressing::Refused { why }
            | Pressing::Unconfirmed { why, .. }
            | Pressing::Lost { why, .. } => why,
        };
        Ok((
            format!("NOT-SERVED {who} {why} (enters=0)"),
            EXIT_NOT_SERVED,
        ))
    }
}

/// Maximum focus-answer attempts when a generation change skips a key, as
/// Muse Code's ticking question header can do. Retries reread the same question
/// and replan the answer; one attempt may send several arrows before Enter.
const FOCUS_TRIES: u32 = 3;

impl<C: Ctl> Session<'_, C> {
    /// Answer the question read by its answers' roles on `screen` that is
    /// chosen by the cursor and Enter (Muse Code's, module header): the focus
    /// moved `focus.steps` options, each move fenced on the generation and
    /// guarded on the question's own row and seen landing, then Enter on the
    /// answer's own row once a fresh read shows the focus on it
    /// ([`Self::press_focused`]) — every key also fenced on the read's person
    /// count, and only while a person has been quiet for the grace by the
    /// read's stamp. A key the generation skipped is tried again on a fresh
    /// read of the same question (its box token), the answer planned on it,
    /// [`FOCUS_TRIES`] attempts in all, keeping the initial person count.
    /// A missing initial count or any read's unknown quiet stamp refuses
    /// that attempt before it writes a key. Returns the reply line and exit code.
    fn answer_focus(
        &mut self,
        who: &str,
        reader: &dyn ScreenReader,
        prompt: &aterm_phase::PromptV2,
        screen: &Screen,
        focus: &Focus,
        opts: &AnswerOpts,
    ) -> Result<(String, u8), String> {
        let left = format!(
            "LEFT {who} a person gave the session input: the question is theirs (enters=0)"
        );
        let changed = "the question changed under the key (a person, or the worker)";
        let person = screen.human_seq;
        let token = prompt_box_token(prompt);
        let (mut prompt, mut screen, mut focus) = (prompt.clone(), screen.clone(), focus.clone());
        let mut tries = 1;
        let why = loop {
            if let HumanInput::Ago(ms) = screen.human
                && u128::from(ms) < opts.grace.as_millis()
            {
                return Ok((left, 0));
            }
            if screen.human == HumanInput::Unknown {
                break "the read carried no quiet stamp to establish the person's grace"
                    .to_string();
            }
            let Some(person) = person else {
                break "the read carried no person count to fence on".to_string();
            };
            break match self
                .press_focused(
                    &prompt,
                    focus.steps,
                    &focus.label,
                    "enter",
                    &focus.guard,
                    &screen,
                    Some(person),
                    Some(super::super::policy::KeyAct::Answer),
                )
                .map_err(fail_text)?
            {
                Pressing::Done(Press::Pressed { seq }) => {
                    let _ = self.wait(&["seq", &seq.to_string()], STRAY_SETTLE);
                    let _ = self.wait(&["idle", SETTLE_MS], SETTLE_CAP);
                    let now = self.screen().map_err(fail_text)?;
                    return Ok((
                        format!(
                            "ANSWERED {who} enters=1 answer={} next={}",
                            focus.said,
                            next_of(reader, &now.rows)
                        ),
                        0,
                    ));
                }
                Pressing::Done(Press::Yielded { .. }) => return Ok((left, 0)),
                Pressing::Done(Press::Changed { .. }) if tries < FOCUS_TRIES => {
                    tries += 1;
                    screen = self.screen().map_err(fail_text)?;
                    let again = reader
                        .prompt(&screen.rows)
                        .filter(|p| p.kind == PromptKind::Question && prompt_box_token(p) == token);
                    let Some(again) = again else {
                        break changed.to_string();
                    };
                    match plan(&again, &screen.rows, &opts.answer, who) {
                        Ok(Plan::Focus(next)) => {
                            prompt = again;
                            focus = next;
                            continue;
                        }
                        _ => changed.to_string(),
                    }
                }
                Pressing::Done(Press::Changed { .. }) => changed.to_string(),
                Pressing::Done(Press::Skipped { .. }) => {
                    "the guarded row was not on the screen when the key went".to_string()
                }
                Pressing::Done(Press::Unseen { .. } | Press::Typed { .. }) => {
                    "the key was not seen through".to_string()
                }
                Pressing::Halted { why }
                | Pressing::Refused { why }
                | Pressing::Unconfirmed { why, .. }
                | Pressing::Lost { why, .. } => why,
            };
        };
        Ok((
            format!("NOT-SERVED {who} {why} (enters=0)"),
            EXIT_NOT_SERVED,
        ))
    }
}

/// A [`Fail`] as the one-shot verb reports it.
fn fail_text(f: Fail) -> String {
    match f {
        Fail::Lost(why) | Fail::Hard(why) => why,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_phase::prompt::fixtures as f;

    fn read(text: &str) -> (aterm_phase::PromptV2, Vec<String>) {
        let rows = f::screen(text);
        let p = aterm_phase::parse_prompt_v2(&rows).expect("a question dialog");
        (p, rows)
    }

    fn planned(text: &str, answer: &str) -> Result<Plan, String> {
        let (p, rows) = read(text);
        plan(&p, &rows, answer, "@s-w")
    }

    /// Single-select: a number or label (case folded) names one option; a word or number
    /// naming none is refused as free text.
    #[test]
    fn a_single_select_answer_names_one_option_or_is_refused() {
        let one = |n| {
            Ok(Plan::Keys {
                targets: vec![AnswerTarget::Option(n)],
                said: ["Dark (Recommended)", "Light", "High contrast"][usize::from(n) - 1]
                    .to_string(),
            })
        };
        assert_eq!(planned(f::QUESTION_ONE, "2"), one(2));
        assert_eq!(planned(f::QUESTION_ONE, " high CONTRAST "), one(3));
        assert_eq!(planned(f::QUESTION_ONE, "Dark (Recommended)"), one(1));
        assert!(matches!(
            planned(f::QUESTION_ONE, "recommended"),
            Ok(Plan::Keys { ref targets, .. }) if targets == &[AnswerTarget::Option(1)]
        ));
        assert_eq!(planned(f::QUESTION_ONE, "human"), Ok(Plan::Human));
        for bad in ["whatever you think is best", "4", "9", "Type something."] {
            planned(f::QUESTION_ONE, bad).expect_err(bad);
        }
    }

    /// A priority word that labels an option answers that option, single- and multi-select
    /// alike; it is refused as a policy only where it names no option.
    #[test]
    fn a_priority_word_that_labels_an_option_answers_that_option() {
        let single = f::QUESTION_ONE.replace("  2. Light", "  2. Long-term");
        for answer in ["long-term", "Long-term", " LONG-TERM "] {
            assert_eq!(
                planned(&single, answer),
                Ok(Plan::Keys {
                    targets: vec![AnswerTarget::Option(2)],
                    said: "Long-term".to_string(),
                }),
                "{answer:?}"
            );
        }
        let err = planned(&single, "short-term").expect_err("no such option");
        assert!(err.contains("meta set questions short-term"), "{err}");
        let multi =
            f::QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED.replace("[ ] Olives", "[ ] Short-term");
        assert_eq!(
            planned(&multi, "short-term"),
            Ok(Plan::Keys {
                targets: vec![AnswerTarget::Option(2), AnswerTarget::Button],
                said: "Short-term".to_string(),
            })
        );
        planned(&multi, "long-term").expect_err("no such option");
    }

    /// `recommended` on a multi-select toggles each unchecked recommended option, then the
    /// button; a person's check outside the recommended set refuses it.
    #[test]
    fn recommended_on_a_multi_select_plans_the_whole_tab() {
        let said = "Which glyph sets should be bundled? → Next (Box drawing (Recommended), \
                    Powerline (Recommended))";
        assert_eq!(
            planned(f::QUESTION_MULTISELECT, "recommended"),
            Ok(Plan::Keys {
                targets: vec![
                    AnswerTarget::Option(1),
                    AnswerTarget::Option(3),
                    AnswerTarget::Button
                ],
                said: said.to_string(),
            })
        );
        assert_eq!(
            planned(f::QUESTION_MULTISELECT, "recommended").map(|p| match p {
                Plan::Keys { targets, .. } => targets,
                Plan::Human | Plan::Digit(_) | Plan::Focus(_) => Vec::new(),
            }),
            planned(f::QUESTION_MULTISELECT, "1, 3").map(|p| match p {
                Plan::Keys { targets, .. } => targets,
                Plan::Human | Plan::Digit(_) | Plan::Focus(_) => Vec::new(),
            }),
            "the explicit answer's keys"
        );
        assert_eq!(
            planned(f::QUESTION_MULTISELECT_CHECKED, "recommended"),
            Ok(Plan::Keys {
                targets: vec![AnswerTarget::Button],
                said: said.to_string(),
            })
        );
        let person = f::QUESTION_MULTISELECT.replace("[ ] Emoji", "[✔] Emoji");
        planned(&person, "recommended").expect_err("a person's check");
    }

    /// Multi-select: each option whose check differs is toggled, then the button. The review
    /// tab takes `submit` only.
    #[test]
    fn a_multi_select_toggles_what_differs_then_presses_the_button() {
        assert_eq!(
            planned(f::QUESTION_MULTISELECT, "1, Powerline (Recommended)"),
            Ok(Plan::Keys {
                targets: vec![
                    AnswerTarget::Option(1),
                    AnswerTarget::Option(3),
                    AnswerTarget::Button
                ],
                said: "Box drawing (Recommended), Powerline (Recommended)".to_string(),
            })
        );
        assert!(planned(f::QUESTION_MULTISELECT, "1, nonsense").is_err());
        assert!(planned(f::QUESTION_MULTISELECT, " , ").is_err());
        assert_eq!(
            planned(f::QUESTION_REVIEW, "Submit"),
            Ok(Plan::Keys {
                targets: vec![AnswerTarget::ReviewSubmit],
                said: "submit".to_string(),
            })
        );
        planned(f::QUESTION_REVIEW, "1").expect_err("not submit");
    }

    /// The token is the same on every read of one screen and differs across
    /// questions and tabs.
    #[test]
    fn the_box_token_names_one_screen() {
        let token = |t: &str| question_box_token(&read(t).0.question_dialog.expect("read whole"));
        assert_eq!(token(f::QUESTION_ONE), token(f::QUESTION_ONE));
        assert_eq!(token(f::QUESTION_ONE).len(), 8);
        let all = [
            token(f::QUESTION_ONE),
            token(f::QUESTION_TABS_INCIDENT),
            token(f::QUESTION_TABS_SECOND),
            token(f::QUESTION_TABS_THIRD),
            token(f::QUESTION_TABS_FOURTH),
            token(f::QUESTION_REVIEW),
            token(f::QUESTION_MULTISELECT),
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "{all:?}");
            }
        }
    }

    /// Muse Code's question (1.4.10), read by its answers' roles, is chosen
    /// by the cursor and Enter — no digit was measured on it — so an answer
    /// is planned as the focus moved to it: `recommended` stays on `Red
    /// (Recommended)`, `2` and `blue` move one down, and from `Blue` focused
    /// `1` moves one up; each guarded on the question's own row, never its
    /// head (whose clock ticks); and in a 60-column tab, where each
    /// description is drawn under its label, `blue` names `Blue` alone.
    /// NEGATIVE CONTROLS: `None of the above` (its
    /// digit or its label) asks for the person's own words and is refused,
    /// and `human` types nothing.
    #[test]
    fn a_muse_question_is_planned_as_the_focus_moved_to_its_answer() {
        use aterm_phase::muse::fixtures as u;
        let read = |text: &str| {
            let rows = f::screen(text);
            let p = aterm_phase::Program::Muse
                .reader()
                .prompt(&rows)
                .expect("the question");
            (p, rows)
        };
        let (p, rows) = read(u::QUESTION);
        let guard = row_guard("  Which color should the probe use?");
        let focus = |steps: i32, label: &str, said: &str| {
            Ok(Plan::Focus(Focus {
                steps,
                label: label.to_string(),
                guard: guard.clone(),
                said: said.to_string(),
            }))
        };
        assert_eq!(
            plan(&p, &rows, "recommended", "@s-w"),
            focus(
                0,
                "Red (Recommended)",
                "Request user input Color → Red (Recommended)"
            )
        );
        for answer in ["2", "blue"] {
            assert_eq!(
                plan(&p, &rows, answer, "@s-w"),
                focus(1, "Blue", "Blue"),
                "{answer}"
            );
        }
        let (down, down_rows) = read(u::QUESTION_DOWN);
        assert_eq!(
            plan(&down, &down_rows, "1", "@s-w"),
            focus(-1, "Red (Recommended)", "Red (Recommended)")
        );
        for answer in ["3", "none of the above"] {
            let err = plan(&p, &rows, answer, "@s-w").expect_err(answer);
            assert!(err.contains("person's own words"), "{answer}: {err}");
        }
        assert_eq!(plan(&p, &rows, "human", "@s-w"), Ok(Plan::Human));
        // Drawn in a 60-column tab (each description under its label, the
        // key row on two rows), `blue` still names its answer.
        let (narrow, narrow_rows) = read(u::QUESTION_WRAPPED);
        assert_eq!(
            plan(&narrow, &narrow_rows, "blue", "@s-w"),
            Ok(Plan::Focus(Focus {
                steps: 1,
                label: "Blue".to_string(),
                guard: row_guard("  Which shade should the probe use for the long label test?"),
                said: "Blue".to_string(),
            }))
        );
    }

    /// A question read by its answers' roles has a token of its own, over its
    /// title, its question and its labels: the same on every read, another
    /// for another question. A dialog's token stays its dialog's. Negative
    /// control: a box that is no question has none.
    #[test]
    fn a_roles_question_has_a_box_token_and_another_box_none() {
        use aterm_phase::muse::fixtures as u;
        let muse = |text: &str| {
            aterm_phase::Program::Muse
                .reader()
                .prompt(&f::screen(text))
                .expect("a Muse Code box")
        };
        let token = prompt_box_token(&muse(u::QUESTION)).expect("a question's token");
        assert_eq!(token.len(), 8);
        assert_eq!(prompt_box_token(&muse(u::QUESTION)), Some(token.clone()));
        let other = u::QUESTION.replace("Which color should", "Which shade should");
        assert_ne!(prompt_box_token(&muse(&other)), Some(token.clone()));
        let relabelled = u::QUESTION.replace("2. Blue", "2. Navy");
        assert_ne!(prompt_box_token(&muse(&relabelled)), Some(token));
        let (claude, _) = read(f::QUESTION_ONE);
        assert_eq!(
            prompt_box_token(&claude),
            claude.question_dialog.as_ref().map(question_box_token)
        );
        assert_eq!(prompt_box_token(&muse(u::BOX_SHELL)), None);
    }
}
