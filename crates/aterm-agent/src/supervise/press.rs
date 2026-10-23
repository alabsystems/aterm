// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor keystrokes on prompt and question boxes.

use super::super::policy::KeyAct;
use super::super::policy::approval::{
    Answer, AnswerTarget, DeclineStep, QuestionPriority, decline_step,
};
use super::super::policy::question::{
    MAX_QUESTION_FOCUS_STEPS, NoteRead, NoteStep, focus_position, focus_row, note_read, note_step,
    option_label, roles_question_key, roles_question_notes, target_focus,
};
use super::approval_loop::PendingKind;
use super::*;
use aterm_phase::{QuestionDialog, QuestionFocus, QuestionForm};

const MAX_DECLINE_STEPS: u32 = 3 + 3;

const MAX_DECLINE_UNSEEN_READS: u32 = 5;

const UNSTAMPED: &str = "this host does not stamp a person's input, and a decline's keystrokes \
                         could cross a person's";

const MAX_NOTE_STEPS: u32 = MAX_QUESTION_FOCUS_STEPS + 3;

const UNSTAMPED_NOTE: &str = "this host does not stamp a person's input, and a note's keystrokes \
                              could cross a person's";

const TAIL_KEYED: &str = "the notes field draws only the tail of the policy text, and the read \
                          does not show that no person keyed since it was written: the head it \
                          does not draw may be theirs";

type NoteState = (Option<usize>, Option<String>);

fn note_state(q: &aterm_phase::PromptV2, rows: &[String]) -> NoteState {
    (
        q.options.iter().position(|o| o.focused),
        roles_question_notes(q, rows).map(|n| n.text),
    )
}

/// What makes a box the same box for a decline ([`Session::press_decline`]).
pub(super) fn decline_box(q: &aterm_phase::PromptV2, rows: &[String]) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        q.kind.name(),
        q.head_off_screen,
        q.command,
        q.foot_note(rows).unwrap_or_default()
    )
}

const fn keystroke_name(step: &DeclineStep) -> &'static str {
    match step {
        DeclineStep::Move { .. } => "arrow",
        DeclineStep::Amend { .. } => "Tab",
        DeclineStep::Type { .. } => "reason",
        DeclineStep::Submit { .. } => "Enter",
    }
}

fn person_quiet(human: HumanInput, grace: Duration) -> bool {
    match human {
        HumanInput::Ago(ms) => u128::from(ms) >= grace.as_millis(),
        HumanInput::Never | HumanInput::NeverFor(_) | HumanInput::Unknown => true,
    }
}

enum Keyed {
    /// Written: the server's seq after it.
    Written(u64),
    /// Not written, and why, as the loop's answer.
    Not(Pressing),
}

enum Moved {
    /// Every move seen landing.
    Landed {
        now: Box<Screen>,
        dialog: Box<QuestionDialog>,
    },
    /// Stopped short, and what the press answers.
    Stopped(Pressing),
}

fn dialog_of(rows: &[String]) -> Option<QuestionDialog> {
    aterm_phase::parse_prompt_v2(rows).and_then(|p| p.question_dialog)
}

fn field_of(dialog: &QuestionDialog) -> Option<(String, bool, Option<bool>)> {
    match &dialog.form {
        QuestionForm::Single { free_text, .. } | QuestionForm::Multi { free_text, .. } => Some((
            free_text.text.clone(),
            free_text.pristine,
            free_text.checked,
        )),
        _ => None,
    }
}

/// What the read after a policy text shows of it ([`read_back`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Back {
    /// The text exactly, the focus on its row, the same question.
    Exact,
    /// A strict prefix of it: still drawing, or its tail not read yet.
    Short,
    /// The field still empty: the text not taken, or not read.
    Empty,
    /// Anything else.
    Other(&'static str),
}

fn read_back(rows: &[String], first: &QuestionDialog, text: &str) -> Back {
    let Some(dialog) = dialog_of(rows).filter(|d| d.same_question(first)) else {
        return Back::Other("the dialog is no longer the question it was written into");
    };
    let Some((field, pristine, checked)) = field_of(&dialog) else {
        return Back::Other("the dialog has no free-text row");
    };
    if dialog.focus() != QuestionFocus::FreeText {
        return Back::Other("the focus left the free-text row");
    }
    let multi = matches!(dialog.form, QuestionForm::Multi { .. });
    if pristine {
        Back::Empty
    } else if field == text {
        if multi && checked != Some(true) {
            Back::Other("its multi-select row reads unticked")
        } else {
            Back::Exact
        }
    } else if !field.is_empty() && text.starts_with(field.as_str()) {
        Back::Short
    } else {
        Back::Other("the free-text row holds other text")
    }
}

fn latched_for(waited: Wait, ms: &str) -> Option<Duration> {
    matches!(waited, Wait::Latched)
        .then(|| ms.parse().ok().map(Duration::from_millis))
        .flatten()
}

impl<C: Ctl> Session<'_, C> {
    /// Answer a question dialog.
    pub(super) fn press_question(
        &mut self,
        answer: &Answer,
        seen: &Screen,
        grace: Duration,
    ) -> Result<Pressing, Fail> {
        let unconfirmed = |why: &str| {
            Ok(Pressing::Unconfirmed {
                why: why.to_string(),
                retry: false,
            })
        };
        let steps = answer.steps();
        if let Some(priority) = answer.typed() {
            return self.press_question_text(steps, priority, seen, grace);
        }
        let Some(target) = answer.enter_target() else {
            return unconfirmed("a question answer that neither writes nor presses anything");
        };
        if steps.unsigned_abs() > MAX_QUESTION_FOCUS_STEPS {
            return unconfirmed("the focus is too far from the option to move it");
        }
        if !self.gen_fence_known()? || seen.generation.is_none() {
            return unconfirmed("this host cannot fence a question's key on the screen generation");
        }
        let Some(first) = dialog_of(&seen.rows) else {
            return Ok(Pressing::Done(Press::Changed { seq: seen.seq }));
        };
        let want = target_focus(target);
        let label = match target {
            AnswerTarget::Option(n) => option_label(&first, n).map(str::to_string),
            AnswerTarget::Button | AnswerTarget::ReviewSubmit | AnswerTarget::OwnText(_) => None,
        };
        let submits_text = match target {
            AnswerTarget::OwnText(_) => true,
            AnswerTarget::Button => {
                matches!(first.form, QuestionForm::Multi { .. })
                    && field_of(&first)
                        .is_some_and(|(_, pristine, checked)| !pristine || checked == Some(true))
            }
            AnswerTarget::Option(_) | AnswerTarget::ReviewSubmit => false,
        };
        if submits_text {
            let key = approval_loop::question_key(&first, &seen.rows);
            let own = match target {
                AnswerTarget::OwnText(p) => Some(p),
                _ => self.question.typed.get(&key).copied(),
            };
            if let Some(why) = self.not_own(&first, &key, own) {
                return unconfirmed(&why);
            }
        }
        let person = self.person_fence(seen);
        let seen = match self.first_key_wait(&first, seen, grace)? {
            Ok(now) => now,
            Err(stop) => return Ok(stop),
        };
        let (now, dialog) = match self.move_focus(steps, &first, &seen, grace, person)? {
            Moved::Landed { now, dialog } => (*now, *dialog),
            Moved::Stopped(pressing) => return Ok(pressing),
        };
        let same_label = match (&label, target) {
            (Some(l), AnswerTarget::Option(n)) => option_label(&dialog, n) == Some(l.as_str()),
            _ => true,
        };
        if dialog.focus() != want || !same_label {
            return unconfirmed("the focus did not land on the option it was moved to");
        }
        if self.text_word().is_some() && field_of(&dialog) != field_of(&first) {
            return unconfirmed("the free-text row changed since the decision read");
        }
        let Some(row) = focus_row(&dialog, want).filter(|&r| r < now.rows.len()) else {
            return unconfirmed("the focused row is not on the rows read");
        };
        if submits_text && person.is_none() {
            return unconfirmed(
                "the read carried no person count to fence the Enter on a typed answer",
            );
        }
        let guard = super::super::policy::row_guard(&now.rows[row]);
        let at = match self.question_key(&guard, "enter", &now, person, Some(KeyAct::Answer))? {
            Keyed::Written(at) => at,
            Keyed::Not(p) => return Ok(p),
        };
        self.question_pending(&now, want, PendingKind::Enter, false);
        Ok(Pressing::Done(Press::Pressed { seq: at }))
    }

    fn text_word(&self) -> Option<QuestionPriority> {
        self.question.deciding.filter(|w| w.text().is_some())
    }

    fn not_own(
        &self,
        dialog: &QuestionDialog,
        key: &str,
        own: Option<QuestionPriority>,
    ) -> Option<String> {
        let Some(p) = own else {
            return Some("no policy text of the supervisor's is in this question".to_string());
        };
        let (Some((text, pristine, checked)), Some(want)) = (field_of(dialog), p.text()) else {
            return Some("the question has no free-text row for a policy text".to_string());
        };
        let multi = matches!(dialog.form, QuestionForm::Multi { .. });
        if self.question.typed.get(key) != Some(&p)
            || self.question.mode != Some(p)
            || self.question.deciding != Some(p)
        {
            Some(format!(
                "the {} policy text is not one the supervisor remembers writing under this \
                 dialog's word",
                p.word()
            ))
        } else if pristine || text != want {
            Some("the free-text row does not hold the policy text exactly".to_string())
        } else if multi && checked != Some(true) {
            Some("the policy text's row reads unticked".to_string())
        } else {
            None
        }
    }

    fn first_key_wait(
        &mut self,
        first: &QuestionDialog,
        seen: &Screen,
        grace: Duration,
    ) -> Result<Result<Screen, Pressing>, Fail> {
        let key_of = approval_loop::question_key(first, &seen.rows);
        if self.question.keyed.as_deref() == Some(key_of.as_str()) {
            return Ok(Ok(seen.clone()));
        }
        self.question.keyed = Some(key_of);
        self.wait(&["idle", QUESTION_FIRST_KEY_IDLE], QUESTION_FIRST_KEY_CAP)?;
        let (stamp, generation) = self.status_now()?;
        if self.driver || stamp.is_some_and(|ms| !person_quiet(HumanInput::Ago(ms), grace)) {
            return Ok(Err(Pressing::Done(Press::Yielded { seq: seen.seq })));
        }
        if generation.is_none_or(|g| seen.generation.as_deref() == Some(g.as_str())) {
            return Ok(Ok(seen.clone()));
        }
        let now = self.screen()?;
        if now.generation.is_none() || dialog_of(&now.rows).as_ref() != Some(first) {
            return Ok(Err(Pressing::Done(Press::Changed { seq: now.seq })));
        }
        Ok(Ok(now))
    }

    fn move_focus(
        &mut self,
        steps: i32,
        first: &QuestionDialog,
        seen: &Screen,
        grace: Duration,
        person: Option<u64>,
    ) -> Result<Moved, Fail> {
        let unconfirmed = |why: &str| {
            Ok(Moved::Stopped(Pressing::Unconfirmed {
                why: why.to_string(),
                retry: false,
            }))
        };
        let arrow = if steps >= 0 { "down" } else { "up" };
        let sign = steps.signum();
        let mut now = seen.clone();
        let mut dialog = first.clone();
        for _ in 0..steps.unsigned_abs() {
            let at_focus = dialog.focus();
            let (Some(from), Some(row)) = (
                focus_position(&dialog, at_focus),
                focus_row(&dialog, at_focus).filter(|&r| r < now.rows.len()),
            ) else {
                return unconfirmed("the dialog shows no focus to move");
            };
            let guard = super::super::policy::row_guard(&now.rows[row]);
            let at = match self.question_key(&guard, arrow, &now, person, None)? {
                Keyed::Written(at) => at,
                Keyed::Not(p) => return Ok(Moved::Stopped(p)),
            };
            self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
            let unmoved = |d: &Option<QuestionDialog>| {
                d.as_ref()
                    .is_some_and(|d| d.same_question(first) && d.focus() == at_focus)
            };
            let mut read = dialog_of(&now.rows);
            let mut settled = false;
            if unmoved(&read) {
                let (quiet, step) = self.still_wait(&now);
                settled = matches!(self.wait(&["idle", quiet], step)?, Wait::Latched);
                now = self.screen()?;
                read = dialog_of(&now.rows);
            }
            let Some(d) = read.filter(|d| d.same_question(first)) else {
                return Ok(Moved::Stopped(Pressing::Done(Press::Changed {
                    seq: now.seq,
                })));
            };
            if d.focus() == at_focus {
                self.question_pending(&now, at_focus, PendingKind::Move, settled);
                return Ok(Moved::Stopped(Pressing::Done(Press::Unseen {
                    seq: now.seq,
                })));
            }
            if focus_position(&d, d.focus()) != Some(from + sign) {
                return unconfirmed("the focus did not land on the row it was moved to");
            }
            dialog = d;
            if !person_quiet(now.human, grace) {
                return Ok(Moved::Stopped(Pressing::Done(Press::Yielded {
                    seq: now.seq,
                })));
            }
        }
        Ok(Moved::Landed {
            now: Box::new(now),
            dialog: Box::new(dialog),
        })
    }

    /// Move focus `steps` rows onto the question's empty free-text row.
    pub(super) fn press_question_text(
        &mut self,
        steps: i32,
        priority: QuestionPriority,
        seen: &Screen,
        grace: Duration,
    ) -> Result<Pressing, Fail> {
        let unconfirmed = |why: &str| {
            Ok(Pressing::Unconfirmed {
                why: why.to_string(),
                retry: false,
            })
        };
        let Some(text) = priority.text() else {
            return unconfirmed("the recommended policy writes no text");
        };
        if steps.unsigned_abs() > MAX_QUESTION_FOCUS_STEPS {
            return unconfirmed("the focus is too far from the free-text row to move it");
        }
        if !self.gen_fence_known()? || seen.generation.is_none() {
            return unconfirmed("this host cannot fence a question's key on the screen generation");
        }
        let person = self.person_fence(seen);
        if !self.send_fence_known()? || person.is_none() {
            return unconfirmed(
                "this host cannot fence a typed answer (send if-gen=, the person count)",
            );
        }
        let Some(first) = dialog_of(&seen.rows) else {
            return Ok(Pressing::Done(Press::Changed { seq: seen.seq }));
        };
        let key = approval_loop::question_key(&first, &seen.rows);
        if self.question.typed.contains_key(&key) {
            return unconfirmed("the supervisor wrote a policy text into this question already");
        }
        let seen = match self.first_key_wait(&first, seen, grace)? {
            Ok(now) => now,
            Err(stop) => return Ok(stop),
        };
        let (now, dialog) = match self.move_focus(steps, &first, &seen, grace, person)? {
            Moved::Landed { now, dialog } => (*now, *dialog),
            Moved::Stopped(pressing) => return Ok(pressing),
        };
        let Some((_, true, checked)) = field_of(&dialog) else {
            return unconfirmed("the free-text row is not empty");
        };
        if dialog.focus() != QuestionFocus::FreeText
            || checked == Some(true)
            || !dialog.same_question(&first)
        {
            return unconfirmed("the focus did not land on the empty free-text row");
        }
        let Some(row) = focus_row(&dialog, QuestionFocus::FreeText).filter(|&r| r < now.rows.len())
        else {
            return unconfirmed("the free-text row is not on the rows read");
        };
        let guard = super::super::policy::row_guard(&now.rows[row]);
        let at = match self.question_text(&guard, text, &now, &key, priority, person)? {
            Keyed::Written(at) => at,
            Keyed::Not(p) => {
                // No answer: the text may be in the field or on its way. Pending, so a box
                // read unchanged after the reconnect is the text not taken, never re-sent.
                if matches!(p, Pressing::Lost { .. }) {
                    self.question_pending(&now, QuestionFocus::FreeText, PendingKind::Text, false);
                }
                return Ok(p);
            }
        };
        self.question_pending(&now, QuestionFocus::FreeText, PendingKind::Text, false);
        self.wait(
            &["seq", &at.to_string()],
            super::turn_end_loop::REACTION_WAIT,
        )?;
        let mut still = latched_for(self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?, SETTLE_MS);
        let mut back = self.screen()?;
        let mut waited = false;
        loop {
            let reading = read_back(&back.rows, &first, text);
            match reading {
                Back::Exact => {
                    self.question.pending = None;
                    self.read_back = still.map(|still| (back.clone(), still));
                    return Ok(Pressing::Done(Press::Typed { seq: back.seq }));
                }
                Back::Short | Back::Empty | Back::Other(_) if !waited => {
                    waited = true;
                    still = latched_for(self.wait(&["idle", IDLE_MS], WAIT_STEP)?, IDLE_MS);
                    back = self.screen()?;
                }
                Back::Short => {
                    return unconfirmed(
                        "the policy text read back short of what was written: a text is never \
                         written twice",
                    );
                }
                Back::Empty => {
                    return unconfirmed(
                        "the dialog did not take the policy text: a text is never written twice",
                    );
                }
                Back::Other(why) => {
                    return unconfirmed(&format!(
                        "the policy text did not read back as written ({why}): a text is never \
                         written twice"
                    ));
                }
            }
        }
    }

    fn status_now(&mut self) -> Result<(Option<u64>, Option<String>), Fail> {
        let r = self.call(&["status"])?;
        if !r.ok() {
            return Ok((None, None));
        }
        self.note_hand(&r.stdout);
        let field = |name| super::escalate::status_field(&r.stdout, name);
        Ok((
            field("human_ms").and_then(|v| v.parse().ok()),
            field("gen").map(str::to_string),
        ))
    }

    fn question_key(
        &mut self,
        guard: &str,
        key: &str,
        now: &Screen,
        person: Option<u64>,
        act: Option<KeyAct>,
    ) -> Result<Keyed, Fail> {
        debug_assert!(
            matches!(key, "up" | "down" | "enter"),
            "a question is keyed with arrows and Enter only, never {key:?}"
        );
        let Some(generation) = now.generation.as_deref() else {
            return Ok(Keyed::Not(Pressing::Unconfirmed {
                why: "the read carried no screen generation to fence on".to_string(),
                retry: false,
            }));
        };
        let args = super::super::policy::key_args(guard, key, Some(generation));
        let args = match person {
            Some(n) => format!("if-human={n} {args}"),
            None => args,
        };
        let args = self.acting(act, args);
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        if let Some(p) = self.refused_press(&r, &args)? {
            if matches!(p, Pressing::Lost { .. }) {
                self.latch_question_word();
            }
            return Ok(Keyed::Not(p));
        }
        Ok(self.written_or_not(&r, now.seq))
    }

    fn question_text(
        &mut self,
        guard: &str,
        text: &str,
        now: &Screen,
        key: &str,
        priority: QuestionPriority,
        person: Option<u64>,
    ) -> Result<Keyed, Fail> {
        let Some(generation) = now.generation.as_deref() else {
            return Ok(Keyed::Not(Pressing::Unconfirmed {
                why: "the read carried no screen generation to fence on".to_string(),
                retry: false,
            }));
        };
        let Some(count) = person else {
            return Ok(Keyed::Not(Pressing::Unconfirmed {
                why: "the read carried no person count to fence the policy text on".to_string(),
                retry: false,
            }));
        };
        debug_assert!(
            QuestionPriority::ALL.iter().any(|p| p.text() == Some(text)),
            "only a policy's own fixed text is ever written into a question"
        );
        let person = format!("if-human={count}");
        let fence = format!("if-gen={generation}");
        let guarded = format!("if={guard}");
        let r = self.call(&["send", &person, &fence, &guarded, "--", text])?;
        let args = format!("{person} {fence} {guarded} (send)");
        let remember = |s: &mut Self| {
            s.question.typed.insert(key.to_string(), priority);
            s.question.typed_person.insert(key.to_string(), count);
            s.latch_question_word();
        };
        if let Some(p) = self.refused_press(&r, &args)? {
            match &p {
                Pressing::Halted { .. } | Pressing::Refused { .. } => {}
                _ => remember(self),
            }
            return Ok(Keyed::Not(p));
        }
        let keyed = self.written_or_not(&r, now.seq);
        if matches!(keyed, Keyed::Written(_)) {
            remember(self);
        }
        Ok(keyed)
    }

    fn written_or_not(&mut self, r: &CtlReply, seen: u64) -> Keyed {
        let at = r.seq().unwrap_or(seen);
        if r.skipped_person() {
            Keyed::Not(Pressing::Done(Press::Yielded { seq: at }))
        } else if r.skipped_changed() {
            Keyed::Not(Pressing::Done(Press::Changed { seq: at }))
        } else if r.skipped() {
            Keyed::Not(Pressing::Done(Press::Skipped { seq: at }))
        } else {
            self.latch_question_word();
            Keyed::Written(at)
        }
    }

    const fn latch_question_word(&mut self) {
        if self.question.mode.is_none() {
            self.question.mode = self.question.deciding;
        }
    }

    fn question_pending(
        &mut self,
        now: &Screen,
        focus: aterm_phase::QuestionFocus,
        kind: PendingKind,
        waited: bool,
    ) {
        let identity = approval_loop::box_identity(&now.rows).unwrap_or_default();
        let retries = self
            .question
            .pending
            .as_ref()
            .filter(|p| p.identity == identity && p.focus == focus && p.kind == kind)
            .map_or(0, |p| p.retries.saturating_add(1));
        self.question.pending = Some(approval_loop::QuestionPending {
            identity,
            focus,
            kind,
            waited,
            unsettled: 0,
            retries,
            backed_off: false,
        });
    }

    /// Whether the server fences a press on the screen generation.
    pub(super) fn gen_fence_known(&mut self) -> Result<bool, Fail> {
        if let Some(known) = self.caps.gen_fence {
            return Ok(known);
        }
        let r = self.call(&["help", "key"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("help key failed: {}", r.stderr.trim())));
        }
        let known = r.ok() && super::super::policy::server_fences_gen(&r.stdout);
        self.caps.gen_fence = Some(known);
        self.caps.act_word = Some(r.ok() && super::super::policy::server_takes_act(&r.stdout));
        Ok(known)
    }

    fn act_option(&self, act: Option<KeyAct>) -> Option<&'static str> {
        act.filter(|_| self.caps.act_word == Some(true))
            .map(KeyAct::option)
    }

    fn acting(&self, act: Option<KeyAct>, args: String) -> String {
        match self.act_option(act) {
            Some(option) => format!("{option} {args}"),
            None => args,
        }
    }

    /// One digit choosing an option, bound to the judged read's row.
    pub(super) fn press_one_guarded(
        &mut self,
        guard: &str,
        digit: &str,
        seen: &Screen,
        act: KeyAct,
    ) -> Result<Pressing, Fail> {
        let cannot = |why: &str| {
            Ok(Pressing::Unconfirmed {
                why: why.to_string(),
                retry: false,
            })
        };
        let Some(person) = seen.human_seq else {
            return cannot("the digit's read carried no person count to fence on");
        };
        if seen.human == HumanInput::Unknown {
            return cannot("the digit's read carried no quiet stamp");
        }
        if !self.gen_fence_known()? {
            return cannot("this host cannot fence the digit on the screen generation");
        }
        let Some(generation) = seen.generation.as_deref() else {
            return cannot("the digit's read carried no screen generation to fence on");
        };
        let person = format!("if-human={person}");
        let (r, args) =
            self.fenced_key_acting(guard, digit, generation, Some(&person), Some(act))?;
        if r.ok() {
            let seq = r.seq().unwrap_or(seen.seq);
            return Ok(Pressing::Done(if r.skipped_person() {
                Press::Yielded { seq }
            } else if r.skipped_changed() {
                Press::Changed { seq }
            } else if r.skipped() {
                Press::Skipped { seq }
            } else {
                Press::Pressed { seq }
            }));
        }
        let why = format!("key {args} failed: {}", r.stderr.trim());
        if self.unserved(&r) {
            return Ok(Pressing::Lost { why });
        }
        if r.is_err("halted") {
            return Ok(Pressing::Halted { why });
        }
        if r.is_err("busy") || r.is_err("rate") {
            return Ok(Pressing::Refused { why });
        }
        if r.unknown_form() {
            return Ok(Pressing::Unconfirmed { why, retry: false });
        }
        Err(Fail::Hard(why))
    }

    /// Answer a focus-driven dialog.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn press_focused(
        &mut self,
        prompt: &PromptV2,
        steps: i32,
        label: &str,
        key: &str,
        guard: &str,
        seen: &Screen,
        person: Option<u64>,
        act: Option<KeyAct>,
    ) -> Result<Pressing, Fail> {
        self.focus_key_tried = false;
        let fenced = |args: String| match person {
            Some(n) => format!("if-human={n} {args}"),
            None => args,
        };
        let cannot = |why: &str| {
            Ok(Pressing::Unconfirmed {
                why: why.to_string(),
                retry: false,
            })
        };
        let unconfirmed = |why: &str| {
            Ok(Pressing::Unconfirmed {
                why: why.to_string(),
                retry: true,
            })
        };
        if usize::try_from(steps.unsigned_abs()).unwrap_or(usize::MAX) >= prompt.options.len() {
            return cannot("the option to move the focus to is not among the box's options");
        }
        if !self.gen_fence_known()? {
            return cannot("this host cannot fence a focus move on the screen generation");
        }
        let arrow = if steps >= 0 { "down" } else { "up" };
        let program = self.program.clone();
        let focused_on =
            move |rows: &[String]| approval_loop::focused_label(program.as_deref(), rows);
        let before = focused_on(&seen.rows);
        let mut now = seen.clone();
        for _ in 0..steps.unsigned_abs() {
            let Some(generation) = now.generation.clone() else {
                return unconfirmed("the read carried no screen generation to fence on");
            };
            let args = fenced(super::super::policy::key_args(
                guard,
                arrow,
                Some(&generation),
            ));
            let mut words: Vec<&str> = vec!["key"];
            words.extend(args.split(' '));
            let r = self.call(&words)?;
            if let Some(p) = self.refused_press(&r, &args)? {
                return Ok(p);
            }
            let at = r.seq().unwrap_or(now.seq);
            if r.skipped_person() {
                return Ok(Pressing::Done(Press::Yielded { seq: at }));
            }
            if r.skipped_changed() {
                return Ok(Pressing::Done(Press::Changed { seq: at }));
            }
            if r.skipped() {
                return Ok(Pressing::Done(Press::Skipped { seq: at }));
            }
            self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
        }
        let reading = aterm_phase::read(self.program.as_deref(), &now.rows, None);
        let focused = reading.prompt.as_ref().and_then(|q| {
            (q.kind == prompt.kind
                && q.command == prompt.command
                && (prompt.kind != PromptKind::Question
                    || roles_question_key(q) == roles_question_key(prompt)))
            .then(|| q.focused())
            .flatten()
        });
        let Some(focused) = focused.filter(|o| o.label == label) else {
            // Same dialog, focus unmoved: the keys went nowhere (a dialog drawn before its
            // input is live takes none), so read and decide again. A focus that moved elsewhere
            // is the manager's.
            if before.is_some() && focused_on(&now.rows) == before {
                return Ok(Pressing::Done(Press::Changed { seq: now.seq }));
            }
            return unconfirmed("the focus did not land on the option it was moved to");
        };
        let grant = |q: &aterm_phase::PromptV2| {
            aterm_phase::reset_row(q, &self.reset_kinds).is_some_and(|row| row.label == label)
        };
        if grant(prompt) && !reading.prompt.as_ref().is_some_and(grant) {
            return Ok(Pressing::Done(Press::Changed { seq: now.seq }));
        }
        let Some(generation) = now.generation.clone() else {
            return unconfirmed("the read carried no screen generation to fence on");
        };
        let enter_guard = super::super::policy::row_guard(&now.rows[focused.row]);
        let args = self.acting(
            act,
            fenced(super::super::policy::key_args(
                &enter_guard,
                key,
                Some(&generation),
            )),
        );
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        self.focus_key_tried = true;
        let r = self.call(&words)?;
        if let Some(p) = self.refused_press(&r, &args)? {
            return Ok(p);
        }
        let at = r.seq().unwrap_or(now.seq);
        Ok(Pressing::Done(if r.skipped_person() {
            Press::Yielded { seq: at }
        } else if r.skipped_changed() {
            Press::Changed { seq: at }
        } else if r.skipped() {
            Press::Skipped { seq: at }
        } else {
            Press::Pressed { seq: at }
        }))
    }

    /// Execute [`super::super::policy::approval::Decision::Decline`].
    pub(super) fn press_decline(
        &mut self,
        refusal: usize,
        text: &str,
        seen: &Screen,
        grace: Duration,
    ) -> Result<Pressing, Fail> {
        let unconfirmed = |why: &str| {
            Ok(Pressing::Unconfirmed {
                why: why.to_string(),
                retry: false,
            })
        };
        if !self.gen_fence_known()? || !self.send_fence_known()? {
            return unconfirmed("this host cannot fence a keystroke on the screen generation");
        }
        let program = self.program.clone();
        let box_on = move |rows: &[String]| {
            aterm_phase::read(program.as_deref(), rows, None)
                .prompt
                .map(|q| {
                    let key = decline_box(&q, rows);
                    (q, key)
                })
        };
        let Some((_, key)) = box_on(&seen.rows) else {
            return Ok(Pressing::Done(Press::Skipped { seq: seen.seq }));
        };
        let person = self.person_fence(seen).map(|n| format!("if-human={n}"));
        let mut now = seen.clone();
        let mut written = 0;
        loop {
            let Some((q, _)) = box_on(&now.rows).filter(|(_, k)| *k == key) else {
                self.decline.pending = None;
                return Ok(Pressing::Done(Press::Skipped { seq: now.seq }));
            };
            let step = decline_step(&q, &now.rows, refusal, text);
            if let Some(p) = self
                .decline
                .pending
                .as_mut()
                .filter(|p| p.key == key && step.as_ref() == Ok(&p.step))
            {
                p.unseen += 1;
                if p.unseen > MAX_DECLINE_UNSEEN_READS {
                    let what = keystroke_name(&p.step);
                    return unconfirmed(&format!(
                        "the box did not show the {what} the decline wrote, and a keystroke is \
                         never written twice"
                    ));
                }
                self.wait(&["seq", &now.seq.to_string()], STRAY_SETTLE)?;
                self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
                now = self.screen()?;
                continue;
            }
            self.decline.pending = None;
            let step = match step {
                Ok(step) => step,
                Err(why) => return unconfirmed(&why),
            };
            match now.human {
                HumanInput::Unknown => return unconfirmed(UNSTAMPED),
                human if !person_quiet(human, grace) => {
                    return Ok(Pressing::Done(Press::Yielded { seq: now.seq }));
                }
                _ => {}
            }
            if written >= MAX_DECLINE_STEPS {
                return unconfirmed("the decline took more keystrokes than a box has");
            }
            let Some(generation) = now.generation.clone() else {
                return unconfirmed("the read carried no screen generation to fence on");
            };
            self.decline.pending = Some(approval_loop::DeclinePending {
                key: key.clone(),
                step: step.clone(),
                unseen: 0,
            });
            let (r, sent) = match &step {
                DeclineStep::Type { guard } => {
                    let fence = format!("if-gen={generation}");
                    let guard = format!("if={guard}");
                    let mut words: Vec<&str> = vec!["send"];
                    words.extend(person.as_deref());
                    words.extend([fence.as_str(), guard.as_str(), "--", text]);
                    let r = self.call(&words)?;
                    (r, format!("{fence} {guard} (send)"))
                }
                DeclineStep::Move { down, guard } => {
                    let key = if *down { "down" } else { "up" };
                    self.fenced_key(guard, key, &generation, person.as_deref())?
                }
                DeclineStep::Amend { guard } => {
                    self.fenced_key(guard, "tab", &generation, person.as_deref())?
                }
                DeclineStep::Submit { guard } => self.fenced_key_acting(
                    guard,
                    "enter",
                    &generation,
                    person.as_deref(),
                    Some(KeyAct::Deny),
                )?,
            };
            if let Some(p) = self.refused_press(&r, &sent)? {
                if !matches!(p, Pressing::Lost { .. }) {
                    self.decline.pending = None;
                }
                return Ok(p);
            }
            let at = r.seq().unwrap_or(now.seq);
            if r.skipped() {
                self.decline.pending = None;
                return Ok(Pressing::Done(if r.skipped_person() {
                    Press::Yielded { seq: at }
                } else if r.skipped_changed() {
                    Press::Changed { seq: at }
                } else {
                    Press::Skipped { seq: at }
                }));
            }
            written += 1;
            if matches!(step, DeclineStep::Submit { .. }) {
                return Ok(Pressing::Done(Press::Pressed { seq: at }));
            }
            self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
        }
    }

    /// Answer a Codex or Muse Code question via its notes field (`Choice::Note`).
    pub(super) fn press_codex_note(
        &mut self,
        priority: QuestionPriority,
        seen: &Screen,
        grace: Duration,
    ) -> Result<Pressing, Fail> {
        let unconfirmed = |why: &str| {
            Ok(Pressing::Unconfirmed {
                why: why.to_string(),
                retry: false,
            })
        };
        let Some(text) = priority.text() else {
            return unconfirmed("the recommended policy writes no note");
        };
        if !self.gen_fence_known()? || !self.send_fence_known()? {
            return unconfirmed("this host cannot fence a keystroke on the screen generation");
        }
        let program = self.program.clone();
        let question_on = move |rows: &[String]| {
            aterm_phase::read(program.as_deref(), rows, None)
                .prompt
                .filter(|p| {
                    p.kind == aterm_phase::PromptKind::Question && p.question_dialog.is_none()
                })
        };
        let Some(first) = question_on(&seen.rows) else {
            return Ok(Pressing::Done(Press::Skipped { seq: seen.seq }));
        };
        let key = roles_question_key(&first);
        let mut now = seen.clone();
        let mut written = 0;
        let mut last: Option<(NoteStep, NoteState)> = None;
        let mut unseen = 0;
        loop {
            let Some(q) = question_on(&now.rows).filter(|q| roles_question_key(q) == key) else {
                return Ok(Pressing::Done(Press::Changed { seq: now.seq }));
            };
            let state = note_state(&q, &now.rows);
            if let Some((step, was)) = &last {
                let drawing = matches!(step, NoteStep::Type { .. })
                    && state
                        .1
                        .as_deref()
                        .is_some_and(|t| t.is_empty() || note_read(text, t) == NoteRead::Part);
                if *was == state || drawing {
                    unseen += 1;
                    if unseen > MAX_DECLINE_UNSEEN_READS {
                        if matches!(step, NoteStep::Open { .. }) {
                            self.question.fell_back.insert(key);
                            return Ok(Pressing::Done(Press::Changed { seq: now.seq }));
                        }
                        return unconfirmed(&format!(
                            "the question did not show the {} the note wrote, and a keystroke is \
                             never written twice",
                            step.name()
                        ));
                    }
                    self.wait(&["seq", &now.seq.to_string()], STRAY_SETTLE)?;
                    self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
                    now = self.screen()?;
                    continue;
                }
            }
            unseen = 0;
            let own = self.question.typed.get(&key).copied();
            let step = match note_step(&q, &now.rows, priority, own) {
                Ok(step) => step,
                Err(why) => return unconfirmed(&why),
            };
            match now.human {
                HumanInput::Unknown => return unconfirmed(UNSTAMPED_NOTE),
                human if !person_quiet(human, grace) => {
                    return Ok(Pressing::Done(Press::Yielded { seq: now.seq }));
                }
                _ => {}
            }
            if written >= MAX_NOTE_STEPS {
                return unconfirmed("the note took more keystrokes than a question has rows");
            }
            if matches!(step, NoteStep::Enter { whole: false, .. })
                && (now.human_seq.is_none()
                    || self.question.typed_person.get(&key).copied() != now.human_seq)
            {
                return unconfirmed(TAIL_KEYED);
            }
            let enter = matches!(step, NoteStep::Enter { .. });
            if enter && self.question.note_entered.as_deref() == Some(key.as_str()) {
                return unconfirmed(
                    "the note was entered once and the question still holds it: an Enter is \
                     never sent twice",
                );
            }
            let Some(generation) = now.generation.clone() else {
                return unconfirmed("the read carried no screen generation to fence on");
            };
            let Some(person) = now.human_seq.map(|n| format!("if-human={n}")) else {
                return unconfirmed(
                    "the read carried no person count to fence the note's next key",
                );
            };
            let keyed = match &step {
                NoteStep::Type { guard } => {
                    self.question_text(guard, text, &now, &key, priority, now.human_seq)?
                }
                NoteStep::Move { down, guard } => {
                    let arrow = if *down { "down" } else { "up" };
                    let (r, args) =
                        self.fenced_key(guard, arrow, &generation, Some(person.as_str()))?;
                    self.note_keyed(&r, &args, now.seq)?
                }
                NoteStep::Open { guard } => {
                    let (r, args) =
                        self.fenced_key(guard, "tab", &generation, Some(person.as_str()))?;
                    self.note_keyed(&r, &args, now.seq)?
                }
                NoteStep::Enter { guard, .. } => {
                    let (r, args) = self.fenced_key_acting(
                        guard,
                        "enter",
                        &generation,
                        Some(person.as_str()),
                        Some(KeyAct::Answer),
                    )?;
                    self.note_keyed(&r, &args, now.seq)?
                }
            };
            let at = match keyed {
                Keyed::Written(at) => at,
                Keyed::Not(p) => {
                    // An Enter whose answer never came may have been written.
                    if enter && matches!(p, Pressing::Lost { .. }) {
                        self.question.note_entered = Some(key);
                    }
                    return Ok(p);
                }
            };
            written += 1;
            if enter {
                self.question.note_entered = Some(key);
                return Ok(Pressing::Done(Press::Pressed { seq: at }));
            }
            last = Some((step, state));
            self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
        }
    }

    fn note_keyed(&mut self, r: &CtlReply, args: &str, seen: u64) -> Result<Keyed, Fail> {
        if let Some(p) = self.refused_press(r, args)? {
            if matches!(p, Pressing::Lost { .. }) {
                self.latch_question_word();
            }
            return Ok(Keyed::Not(p));
        }
        Ok(self.written_or_not(r, seen))
    }

    /// `key [if-human=<n>] if-gen=<generation> if=<guard> <key>`.
    pub(super) fn fenced_key(
        &mut self,
        guard: &str,
        key: &str,
        generation: &str,
        person: Option<&str>,
    ) -> Result<(CtlReply, String), Fail> {
        self.fenced_key_acting(guard, key, generation, person, None)
    }

    /// [`Self::fenced_key`] for a key that dismisses a box.
    pub(super) fn fenced_key_acting(
        &mut self,
        guard: &str,
        key: &str,
        generation: &str,
        person: Option<&str>,
        act: Option<KeyAct>,
    ) -> Result<(CtlReply, String), Fail> {
        let args = super::super::policy::key_args(guard, key, Some(generation));
        let args = match person {
            Some(fence) => format!("{fence} {args}"),
            None => args,
        };
        let args = self.acting(act, args);
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        Ok((r, args))
    }

    /// A fenced press the server did not take, as a [`Pressing`].
    pub(super) fn refused_press(
        &mut self,
        r: &CtlReply,
        args: &str,
    ) -> Result<Option<Pressing>, Fail> {
        if r.ok() {
            return Ok(None);
        }
        let why = format!("key {args} failed: {}", r.stderr.trim());
        Ok(Some(if self.unserved(r) {
            Pressing::Lost { why }
        } else if r.is_err("halted") {
            Pressing::Halted { why }
        } else if r.is_err("busy") || r.is_err("rate") {
            Pressing::Refused { why }
        } else {
            Pressing::Unconfirmed { why, retry: false }
        }))
    }

    /// Dismiss the survey prompt (`key [if-human=<n>] if=<guard> 0`).
    pub(super) fn dismiss_survey(
        &mut self,
        seen: u64,
        guard: &str,
        human_seq: Option<u64>,
    ) -> Result<Dismiss, Fail> {
        self.guarded_key(seen, guard, "0", human_seq)
    }

    /// Press `key` under a row guard (`key [if-human=<n>] if=<guard> <key>`) without `if-gen=`.
    pub(super) fn guarded_key(
        &mut self,
        seen: u64,
        guard: &str,
        key: &str,
        human_seq: Option<u64>,
    ) -> Result<Dismiss, Fail> {
        let person = human_seq.map(|n| format!("if-human={n}"));
        let cond = format!("if={guard}");
        let mut words: Vec<&str> = vec!["key"];
        words.extend(person.as_deref());
        words.extend([cond.as_str(), key]);
        let mut busy = 0;
        loop {
            let r = self.call(&words)?;
            if r.ok() {
                return Ok(if r.skipped_person() {
                    Dismiss::Yielded
                } else if r.skipped() {
                    Dismiss::Skipped
                } else {
                    Dismiss::Pressed {
                        seq: r.seq().unwrap_or(seen),
                    }
                });
            }
            let why = format!("{} failed: {}", words.join(" "), r.stderr.trim());
            if self.unserved(&r) {
                return Err(Fail::Lost(why));
            }
            if r.is_err("busy sink") && busy < BUSY_SINK_RETRIES {
                busy += 1;
                self.fx.sleep(BUSY_SINK_BACKOFF);
                continue;
            }
            if r.is_err("halted") {
                return Ok(Dismiss::Refused { halted: true, why });
            }
            if r.is_err("busy") || r.is_err("rate") {
                return Ok(Dismiss::Refused { halted: false, why });
            }
            return Err(Fail::Hard(why));
        }
    }

    /// Send `key backspace` to remove a stray digit from the composer.
    pub(super) fn backspace(&mut self) -> Result<Backspaced, Fail> {
        let mut busy = 0;
        loop {
            let r = self.call(&["key", "backspace"])?;
            if r.ok() {
                return Ok(Backspaced::Done);
            }
            let why = format!("key backspace failed: {}", r.stderr.trim());
            if self.unserved(&r) {
                return Err(Fail::Lost(why));
            }
            if r.is_err("busy sink") && busy < BUSY_SINK_RETRIES {
                busy += 1;
                self.fx.sleep(BUSY_SINK_BACKOFF);
                continue;
            }
            if r.is_err("halted") {
                return Ok(Backspaced::Refused { halted: true, why });
            }
            if r.is_err("busy") || r.is_err("rate") {
                return Ok(Backspaced::Refused { halted: false, why });
            }
            return Err(Fail::Hard(why));
        }
    }
}
