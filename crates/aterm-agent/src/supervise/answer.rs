// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm drive answer @<worker> [--box TOKEN] <answer>`: a CONTROLLING
//! session types ITS OWN choice into a worker's Claude Code question dialog
//! (AskUserQuestion) — the owner's "if being explicitly controlled via
//! another aterm session … get the selection by the other controller
//! session" (2026-09-23). The worker's window hands the dialog over when the
//! session's word is `ask` (`aterm ctl @<worker> meta set questions ask`);
//! the controller reads it (`aterm drive phase`: its `kind question`, its
//! options, its `box <token>`) and answers it with this.
//!
//! One SCREEN at a time — the tab on display — through the window
//! supervisor's own keys ([`Session::press_question`]): the focus moved one
//! row a key, each move seen landing on a fresh read, then Enter guarded on
//! the chosen row as drawn under the screen-generation fence. Never a digit,
//! Esc, `n`, or a key on the free-text row, the chat row or the review's
//! cancel; a person who keys the session meanwhile gets it (`LEFT`). The
//! answer:
//!
//! * `<n>` or an option's exact label (case folded) — single-select and the
//!   preview form: that option;
//! * a multi-select: the options to CHECK, `, `-joined (numbers or labels) —
//!   each whose check differs is toggled, then the button (`Next`/`Submit`);
//! * `submit` on the review (Submit) tab;
//! * `recommended` — what the window itself would press
//!   ([`super::super::policy::question::answer_question`]);
//! * `human` — nothing typed, `LEFT`.
//!
//! Anything else is REFUSED: a free-text answer is a person's to type (the
//! window's rules keep every key off that row). `--box TOKEN` refuses a
//! dialog whose token ([`question_box_token`]) is not TOKEN — the one the
//! controller read — so an answer never lands on another question.
//!
//! Prints ONE line: `ANSWERED @<w> enters=<k> answer=<what> next=<the question
//! on display after it|review|none>` (exit 0), `LEFT @<w> <why>` (exit 0),
//! `NO-BOX @<w> …` (exit 1), `REFUSED @<w> <why>` (exit 2, nothing typed),
//! `NOT-SERVED @<w> <why> (enters=<k>)` (exit 3: the host or the dialog did not
//! take the answer as decided; the focus may have moved, and enters>0 answered
//! part of it).

use std::io::Write;

use aterm_phase::{QuestionDialog, QuestionForm, QuestionOption};

use super::super::policy::approval::{Answer, AnswerTarget, Choice, Decision};
use super::super::policy::question::{answer_question, focus_position, target_focus};
use super::*;

/// No question dialog on the screen.
pub const EXIT_NO_BOX: u8 = 1;
/// The answer does not fit the dialog, or `--box` names another: nothing
/// typed.
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
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", (h ^ (h >> 32)) & 0xffff_ffff)
}

/// What one screen's answer is, planned against the dialog as read.
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

/// Plan `answer` on the dialog on display (module header). `Err` is the
/// refusal: nothing is typed.
fn plan(prompt: &aterm_phase::PromptV2, rows: &[String], answer: &str) -> Result<Plan, String> {
    let Some(dialog) = prompt.question_dialog.as_ref() else {
        return Err("the dialog was not read whole".to_string());
    };
    let answer = answer.trim();
    if answer.eq_ignore_ascii_case("human") {
        return Ok(Plan::Human);
    }
    if answer.eq_ignore_ascii_case("recommended") {
        return match answer_question(prompt, rows) {
            Decision::Approve {
                choice: Choice::Answer(Answer::FocusEnter { target, .. }),
                subject,
                ..
            } => Ok(Plan::Keys {
                targets: vec![target],
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
    let free_text =
        || format!("{answer:?} names no option: a free-text answer is a person's to type");
    match &dialog.form {
        QuestionForm::Single { options, .. } | QuestionForm::Preview { options, .. } => {
            let o = option_named(options, answer).ok_or_else(free_text)?;
            Ok(Plan::Keys {
                targets: vec![AnswerTarget::Option(o.n)],
                said: o.label.clone(),
            })
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
        QuestionForm::Review { .. } => Err(format!(
            "{answer:?} on the review tab: `submit` sends the answers shown, `human` leaves it"
        )),
        QuestionForm::Unknown { why } => Err(format!("the dialog was not read whole: {why}")),
    }
}

/// The question on display, for `next=`: its text, `review`, or `none`.
fn next_of(rows: &[String]) -> String {
    match aterm_phase::parse_prompt_v2(rows).and_then(|p| p.question_dialog) {
        Some(d) => match (&d.form, d.question()) {
            (QuestionForm::Review { .. }, _) => "review".to_string(),
            (_, Some(q)) => q.text.clone(),
            _ => "question".to_string(),
        },
        None => "none".to_string(),
    }
}

/// `aterm drive answer`'s options.
#[derive(Debug, Clone)]
pub struct AnswerOpts {
    /// The controller's answer (module header).
    pub answer: String,
    /// `--box`: the dialog's token the answer was decided on.
    pub box_token: Option<String>,
    /// A person's quiet before a key goes (`[harness] human_grace_s`): one
    /// who keyed within it gets the dialog (`LEFT`).
    pub grace: Duration,
}

impl<C: Ctl> Session<'_, C> {
    /// Answer the question dialog on display (module header): one line to
    /// `out`, the exit code back.
    pub fn answer(&mut self, opts: &AnswerOpts, out: &mut dyn Write) -> Result<u8, String> {
        let who = self.sid.clone().unwrap_or_else(|| "@self".to_string());
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
        let Some(prompt) = aterm_phase::parse_prompt_v2(&screen.rows)
            .filter(|p| p.kind == aterm_phase::PromptKind::Question)
        else {
            return say(
                format!("NO-BOX {who} no question dialog on the screen"),
                EXIT_NO_BOX,
            );
        };
        if let (Some(want), Some(dialog)) = (&opts.box_token, prompt.question_dialog.as_ref()) {
            let on = question_box_token(dialog);
            if !on.eq_ignore_ascii_case(want.trim()) {
                return say(
                    format!(
                        "REFUSED {who} the dialog on the screen is box={on}, not the box={} the \
                         answer was for: nothing typed",
                        want.trim()
                    ),
                    EXIT_REFUSED,
                );
            }
        }
        let (targets, said) = match plan(&prompt, &screen.rows, &opts.answer) {
            Ok(Plan::Human) => return say(format!("LEFT {who} human: nothing typed"), 0),
            Ok(Plan::Keys { targets, said }) => (targets, said),
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
                Pressing::Withheld => "the host has no guarded key".to_string(),
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
                next_of(&screen.rows)
            ),
            0,
        )
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
        plan(&p, &rows, answer)
    }

    /// Single-select: a number or an exact label (case folded) names the
    /// option; `recommended` is what the window would press; `human` types
    /// nothing. NEGATIVE CONTROLS: a word that names no option is refused as
    /// free text, and a number past the options is too.
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
            let err = planned(f::QUESTION_ONE, bad).expect_err(bad);
            assert!(
                err.contains("a free-text answer is a person's"),
                "{bad}: {err}"
            );
        }
    }

    /// Multi-select: the options to CHECK — each whose check differs is
    /// toggled, then the button; labels and numbers mix. The review tab takes
    /// `submit` only.
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
        let err = planned(f::QUESTION_REVIEW, "1").expect_err("not submit");
        assert!(err.contains("`submit` sends the answers shown"), "{err}");
    }

    /// The token is the same on every read of one screen and differs across
    /// questions and tabs (the four-tab incident's tabs, the review).
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
}
