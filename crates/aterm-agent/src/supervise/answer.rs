// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm drive answer @<worker> [--box TOKEN] <answer>`.

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

/// The dialog's TOKEN.
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

fn role_box_token(prompt: &aterm_phase::PromptV2) -> String {
    let mut name = format!("roles\u{1f}{}\u{1e}{}", prompt.title, prompt.description);
    for o in &prompt.options {
        name.push('\u{1f}');
        name.push_str(&o.label);
    }
    token_of(&name)
}

/// The token `phase` prints as `box` and `--box` names.
#[must_use]
pub fn prompt_box_token(prompt: &aterm_phase::PromptV2) -> Option<String> {
    match &prompt.question_dialog {
        Some(dialog) => Some(question_box_token(dialog)),
        None => (prompt.kind == PromptKind::Question
            && prompt.options.iter().any(|o| o.role == Role::Answer))
        .then(|| role_box_token(prompt)),
    }
}

fn token_of(name: &str) -> String {
    let h = aterm_hash::fnv1a_64(name.as_bytes());
    format!("{:08x}", (h ^ (h >> 32)) & 0xffff_ffff)
}

/// What one screen's answer is, planned against the question as read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    /// Nothing typed: the person answers.
    Human,
    /// Enter on each target in turn (a multi-select's toggles, then its button).
    Keys {
        targets: Vec<AnswerTarget>,
        said: String,
    },
    /// The digit of a question read by its answers' roles.
    Digit(Digit),
    /// The focus moved to an answer of a question read by its answers' roles that is chosen by the
    /// cursor and Enter (Muse Code's).
    Focus(Focus),
}

/// An answer's digit on a question read by its answers' roles (agy's).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Digit {
    n: u8,
    guard: String,
    said: String,
}

/// An answer of a question read by its answers' roles that is chosen by the cursor and Enter.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Focus {
    steps: i32,
    label: String,
    guard: String,
    said: String,
}

fn option_named<'a>(options: &'a [QuestionOption], word: &str) -> Option<&'a QuestionOption> {
    let word = word.trim();
    if let Ok(n) = word.parse::<u8>() {
        return options.iter().find(|o| o.n == n);
    }
    options
        .iter()
        .find(|o| o.label.trim().eq_ignore_ascii_case(word))
}

fn policy_refusal(answer: &str, who: &str, p: QuestionPriority) -> String {
    format!(
        "{answer:?} is a question policy, not an answer: `aterm ctl {who} meta set questions \
         {}` has the window answer this session's questions by it (an answer here is an \
         option, `submit`, `recommended` or `human`)",
        p.word()
    )
}

fn names_no_option(answer: &str, who: &str) -> String {
    match QuestionPriority::parse(answer).filter(|p| p.text().is_some()) {
        Some(p) => policy_refusal(answer, who, p),
        None => format!("{answer:?} names no option: a free-text answer is a person's to type"),
    }
}

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
    /// The controller's answer.
    pub answer: String,
    /// `--box`: the question's token the answer was decided on.
    pub box_token: Option<String>,
    /// A person's quiet before a key goes (`[harness] human_grace_s`).
    pub grace: Duration,
}

impl<C: Ctl> Session<'_, C> {
    /// Answer the question on display (module header): one line to `out`, the exit code back.
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

const FOCUS_TRIES: u32 = 3;

impl<C: Ctl> Session<'_, C> {
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

    #[test]
    fn the_box_token_names_one_screen() {
        let token = |t: &str| question_box_token(&read(t).0.question_dialog.expect("read whole"));
        for fixture in [
            f::QUESTION_ONE,
            f::QUESTION_TABS_INCIDENT,
            f::QUESTION_TABS_SECOND,
            f::QUESTION_TABS_THIRD,
            f::QUESTION_TABS_FOURTH,
            f::QUESTION_REVIEW,
            f::QUESTION_MULTISELECT,
        ] {
            let t = token(fixture);
            assert_eq!(t, token(fixture));
            assert_eq!(t.len(), 8);
            assert!(
                t.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "{t:?}"
            );
        }
        let relabelled_option = f::QUESTION_ONE.replace("Red (Recommended)", "Crimson");
        assert_eq!(
            token(&relabelled_option),
            token(f::QUESTION_ONE),
            "option labels do not move a question_dialog's box token"
        );
    }

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

    #[test]
    fn a_roles_question_has_a_box_token_and_another_box_none() {
        use aterm_phase::muse::fixtures as u;
        let muse = |text: &str| {
            aterm_phase::Program::Muse
                .reader()
                .prompt(&f::screen(text))
                .expect("a Muse Code box")
        };
        let q = muse(u::QUESTION);
        let token = prompt_box_token(&q).expect("a question's token");
        assert_eq!(token.len(), 8);
        assert!(token.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
        assert_eq!(prompt_box_token(&muse(u::QUESTION)), Some(token.clone()));
        let mut refocused = q;
        for opt in &mut refocused.options {
            opt.focused = !opt.focused;
        }
        assert_eq!(
            prompt_box_token(&refocused),
            Some(token),
            "focus changes do not move a role question's box token"
        );
        let (claude, _) = read(f::QUESTION_ONE);
        assert_eq!(
            prompt_box_token(&claude),
            claude.question_dialog.as_ref().map(question_box_token)
        );
        assert_eq!(prompt_box_token(&muse(u::BOX_SHELL)), None);
    }
}
