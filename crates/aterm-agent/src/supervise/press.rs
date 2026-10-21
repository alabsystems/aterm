// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor keystrokes on a prompt box:
//! - approving digit ([`Session::press_one_guarded`])
//! - focus movement and Enter on unnumbered dialogs ([`Session::press_focused`])
//! - question dialog answers ([`Session::press_question`], [`Session::press_question_text`], [`Session::press_codex_note`])
//! - declines ([`Session::press_decline`])
//! - survey dismissal (`0`, [`Session::dismiss_survey`])
//!
//! Option-choosing presses guard on the anchored row ([`super::super::policy::row_guard`])
//! and fence on the screen generation (`if-gen=<epoch>.<seq>`) and person input
//! count (`if-human=<n>`), all checked under the server's terminal write lock.
//! `OK skipped reason=changed` triggers a re-read and re-decision (escalating after
//! [`MAX_CHANGED_PRESSES`] skips); `ERR halted …` ([`Pressing::Halted`]) parks until
//! the hold lifts; `ERR busy …` and `ERR rate` ([`Pressing::Refused`]) back off.

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

/// The most keystrokes a decline takes: the focus moves across a permission
/// box's options to its refusal (three at most; the Bash box's `Yes` to `No`
/// is one), then Tab, the text and Enter.
const MAX_DECLINE_STEPS: u32 = 3 + 3;

/// How many fresh reads after a decline keystroke may still show the box as
/// it was keyed (its effect not drawn yet) before the box is handed over
/// ([`Session::press_decline`]). Each is a wait for the next change (at most
/// [`STRAY_SETTLE`]) and a settle: about ten seconds on a box that holds
/// still. The keystroke is never written again: one not drawn may still be
/// in flight.
const MAX_DECLINE_UNSEEN_READS: u32 = 5;

/// Why a decline is not carried out on a host that stamps no person's input.
const UNSTAMPED: &str = "this host does not stamp a person's input, and a decline's keystrokes \
                         could cross a person's";

/// The most keystrokes a note takes ([`Session::press_codex_note`]): the
/// focus moved across a question's answers onto `None of the above` (as far
/// as a question's focus is ever moved, [`MAX_QUESTION_FOCUS_STEPS`]), then
/// Tab, the text and Enter.
const MAX_NOTE_STEPS: u32 = MAX_QUESTION_FOCUS_STEPS + 3;

/// Why a note is not written on a host that stamps no person's input.
const UNSTAMPED_NOTE: &str = "this host does not stamp a person's input, and a note's keystrokes \
                              could cross a person's";

/// Why a note whose notes field draws only its tail ([`NoteRead::Tail`]) is
/// not entered: the read's person count is not the one the text was written
/// on (a person keyed since, or the read carries none), and the head the
/// field does not draw may hold their keys.
const TAIL_KEYED: &str = "the notes field draws only the tail of the policy text, and the read \
                          does not show that no person keyed since it was written: the head it \
                          does not draw may be theirs";

/// What a note's keystroke is judged landed by: the focused answer's index
/// and the notes field's text (`None` while it is closed).
type NoteState = (Option<usize>, Option<String>);

/// The state of the question `q` read from `rows` ([`NoteState`]).
fn note_state(q: &aterm_phase::PromptV2, rows: &[String]) -> NoteState {
    (
        q.options.iter().position(|o| o.focused),
        roles_question_notes(q, rows).map(|n| n.text),
    )
}

/// What makes a box the same box for a decline ([`Session::press_decline`]):
/// its kind, whether its head is off the screen, its command, and the
/// vendor's note at its foot ([`aterm_phase::PromptV2::foot_note`]: a box
/// taller than the screen has no command to tell it by). A readable box of
/// another kind, or a whole box with the same note, is another box.
pub(super) fn decline_box(q: &aterm_phase::PromptV2, rows: &[String]) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        q.kind.name(),
        q.head_off_screen,
        q.command,
        q.foot_note(rows).unwrap_or_default()
    )
}

/// A decline keystroke, as its hand-over reason names it.
fn keystroke_name(step: &DeclineStep) -> &'static str {
    match step {
        DeclineStep::Move { .. } => "arrow",
        DeclineStep::Amend { .. } => "Tab",
        DeclineStep::Type { .. } => "reason",
        DeclineStep::Submit { .. } => "Enter",
    }
}

/// Whether a read's person stamp lets a question key go (R1): no person
/// ever keyed the session, or they have been quiet at least `grace`
/// (`[harness] human_grace_s`). A host that sends no stamp is judged by the
/// loop's stand-in on the decision read (`approval_loop.rs`), not here.
fn person_quiet(human: HumanInput, grace: Duration) -> bool {
    match human {
        HumanInput::Ago(ms) => u128::from(ms) >= grace.as_millis(),
        HumanInput::Never | HumanInput::NeverFor(_) | HumanInput::Unknown => true,
    }
}

/// One fenced question key's reply, as the question press reads it.
enum Keyed {
    /// Written: the server's seq after it.
    Written(u64),
    /// Not written, and why, as the loop's answer.
    Not(Pressing),
}

/// Where [`Session::move_focus`] left the answer.
enum Moved {
    /// Every move seen landing: the read that showed the last, and its
    /// dialog (`steps` 0: the decision read's).
    /// (Boxed: a read and a dialog dwarf every other variant.)
    Landed {
        now: Box<Screen>,
        dialog: Box<QuestionDialog>,
    },
    /// Stopped short, and what the press answers.
    Stopped(Pressing),
}

/// The question dialog on `rows`, read whole or not at all.
fn dialog_of(rows: &[String]) -> Option<QuestionDialog> {
    aterm_phase::parse_prompt_v2(rows).and_then(|p| p.question_dialog)
}

/// A question tab's free-text row as the reader joins it: its text, whether
/// it is pristine (the placeholder), and its checkbox (multi-select). `None`
/// on a form without one.
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
    /// The text exactly, the focus on its row, the same question — ticked
    /// on a multi-select.
    Exact,
    /// A strict prefix of it: still drawing, or its head read and its tail
    /// not yet.
    Short,
    /// The field still empty: the text not taken, or not read yet.
    Empty,
    /// Anything else, and what.
    Other(&'static str),
}

/// The free-text row of the question `first` asked, on `rows` read after
/// `text` was written into it.
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

/// How long the screen had held still when an `await idle <ms>` answered
/// `waited`: `ms` when it latched (the server measures the window from the
/// wait's arming, so a latch is `ms` of quiet at least), `None` when it ran
/// out or the host has no such wait.
fn latched_for(waited: Wait, ms: &str) -> Option<Duration> {
    matches!(waited, Wait::Latched)
        .then(|| ms.parse().ok().map(Duration::from_millis))
        .flatten()
}

impl<C: Ctl> Session<'_, C> {
    /// Answer a question dialog ([`Answer::FocusEnter`]; [`Answer::Type`] delegates
    /// to [`Self::press_question_text`]): move focus `steps` rows in focus order
    /// and press Enter on `target`, fencing every key on `if-gen=`, `if-human=`,
    /// and the focused row's guard (`if=`).
    ///
    /// Waits [`QUESTION_FIRST_KEY_IDLE`] before the first key on a question tab
    /// (Claude Code ignores keys during a post-mount typeahead window), keeps at
    /// most one key in flight, and verifies [`Self::not_own`] before any Enter
    /// that submits free-text input. Records the last sent key in
    /// [`approval_loop::QuestionPending`].
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
        // The policy text is its own press: never a plain move and Enter.
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
        // An Enter that submits a policy text (on the field, single-select, or
        // on a multi-select's button while its row holds text) goes only while
        // the text is the loop's own (module docs of `policy/question.rs`, the
        // policy text): judged on the decision read, before any key, so an
        // answer it refuses moves nothing. An Enter on a single-select's option
        // submits no text, whatever the field holds (a controller's answer,
        // `aterm drive answer`). The recommended decider never decides such an
        // Enter, so under `recommended` this asks nothing.
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
        if let Some(stop) = self.first_key_wait(&first, seen, grace)? {
            return Ok(stop);
        }
        let (now, dialog) = match self.move_focus(steps, &first, seen, grace)? {
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
        // Under a text priority, the free-text row as the decision read it
        // (pristine on a tab that falls back, the loop's own text on a text
        // answer, the ownership rule judged on it above) is what the Enter
        // goes on: a field that changed since (a person's keys, a multi-select
        // row unticked) gets no Enter. Under `recommended` nothing more is
        // asked.
        if self.text_word().is_some() && field_of(&dialog) != field_of(&first) {
            return unconfirmed("the free-text row changed since the decision read");
        }
        let Some(row) = focus_row(&dialog, want).filter(|&r| r < now.rows.len()) else {
            return unconfirmed("the focused row is not on the rows read");
        };
        // An Enter that submits the policy text is fenced on the person like
        // the text itself: a read with no count cannot fence it, and gets no
        // Enter (the text was written on a read that had one).
        if submits_text && now.human_seq.is_none() {
            return unconfirmed(
                "the read carried no person count to fence the Enter on a typed answer",
            );
        }
        let guard = super::super::policy::row_guard(&now.rows[row]);
        let at = match self.question_key(&guard, "enter", &now, Some(KeyAct::Answer))? {
            Keyed::Written(at) => at,
            Keyed::Not(p) => return Ok(p),
        };
        self.question_pending(&now, want, PendingKind::Enter, false);
        Ok(Pressing::Done(Press::Pressed { seq: at }))
    }

    /// The text word the dialog in hand is answered under: the decision's
    /// ([`approval_loop::QuestionMemory::deciding`]) when it is `long-term` or
    /// `short-term`, its tabs that fall back included; `None` under
    /// `recommended` and for a controller's answer (`aterm drive answer`).
    fn text_word(&self) -> Option<QuestionPriority> {
        self.question.deciding.filter(|w| w.text().is_some())
    }

    /// The ownership rule (module docs of `policy/question.rs`, the policy
    /// text): why the text in `dialog`'s free-text row is not the loop's own
    /// for priority `own`, or `None` when it is: the loop remembers writing
    /// exactly that priority's text into this question
    /// ([`approval_loop::QuestionMemory::typed`]), it is the dialog's latched
    /// word and the word decided on, the field reads it back byte for byte on
    /// the reader's one-space join, and on a multi-select its row reads
    /// ticked.
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

    /// O4: a tab's first key waits for it to have been drawn, still, about a
    /// second ([`QUESTION_FIRST_KEY_IDLE`], once per question), since 2.1.282
    /// refuses keys inside a typeahead window after a dialog mounts; the
    /// person's stamp is asked again after it (`status`'s `human_ms=`), since
    /// a person may have keyed during the wait. `Some` to stop there
    /// ([`Press::Yielded`]). The key after the wait is still fenced on the
    /// generation decided on.
    fn first_key_wait(
        &mut self,
        first: &QuestionDialog,
        seen: &Screen,
        grace: Duration,
    ) -> Result<Option<Pressing>, Fail> {
        let key_of = approval_loop::question_key(first, &seen.rows);
        if self.question.keyed.as_deref() == Some(key_of.as_str()) {
            return Ok(None);
        }
        self.question.keyed = Some(key_of);
        self.wait(&["idle", QUESTION_FIRST_KEY_IDLE], QUESTION_FIRST_KEY_CAP)?;
        Ok(self
            .person_stamp_now()?
            .is_some_and(|ms| !person_quiet(HumanInput::Ago(ms), grace))
            .then_some(Pressing::Done(Press::Yielded { seq: seen.seq })))
    }

    /// Move the focus `steps` rows in `first`'s focus order (down when
    /// positive), one key in flight: each move fenced on the read it was
    /// decided on and guarded on the focused row as drawn, its effect seen on
    /// a settled read before the next ([`Moved::Landed`] with that read and
    /// its dialog; `steps` 0 lands on `seen` itself). A move not seen yet is
    /// waited for once more (`await idle 2000`); still not seen, nothing is
    /// re-sent ([`Press::Unseen`]). A move that landed anywhere but one row
    /// on, a dialog no longer the same question, or a person keying since
    /// (the stamp on the read inside the move), stops it ([`Moved::Stopped`]).
    fn move_focus(
        &mut self,
        steps: i32,
        first: &QuestionDialog,
        seen: &Screen,
        grace: Duration,
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
            let at = match self.question_key(&guard, arrow, &now, None)? {
                Keyed::Written(at) => at,
                Keyed::Not(p) => return Ok(Moved::Stopped(p)),
            };
            // One key in flight: its effect seen before the next.
            self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
            let unmoved = |d: &Option<QuestionDialog>| {
                d.as_ref()
                    .is_some_and(|d| d.same_question(first) && d.focus() == at_focus)
            };
            let mut read = dialog_of(&now.rows);
            // Whether the screen held still after the key (the retry's
            // premise): only a latched wait says so.
            let mut settled = false;
            if unmoved(&read) {
                settled = matches!(self.wait(&["idle", IDLE_MS], WAIT_STEP)?, Wait::Latched);
                now = self.screen()?;
                read = dialog_of(&now.rows);
            }
            let Some(d) = read.filter(|d| d.same_question(first)) else {
                // The dialog changed under the move (a person answered, the
                // tab moved on): read and decided again.
                return Ok(Moved::Stopped(Pressing::Done(Press::Changed {
                    seq: now.seq,
                })));
            };
            if d.focus() == at_focus {
                // Not seen landing: the key may still be in flight. Nothing is
                // re-sent; the loop waits on it, at once when the screen held
                // still after it, else for it to.
                self.question_pending(&now, at_focus, PendingKind::Move, settled);
                return Ok(Moved::Stopped(Pressing::Done(Press::Unseen {
                    seq: now.seq,
                })));
            }
            if focus_position(&d, d.focus()) != Some(from + sign) {
                return unconfirmed("the focus did not land on the row it was moved to");
            }
            dialog = d;
            // The next key is fenced on this read: a person who keyed since
            // the decision read (its own stamp says so) gets the dialog, and
            // nothing more is sent until they are quiet (R1).
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

    /// Move focus `steps` rows onto the question's empty free-text row, write
    /// `priority`'s fixed text in a single fenced `send` (`if-human=`, `if-gen=`,
    /// `if=`), and verify the read-back matches byte-for-byte before returning
    /// [`Press::Typed`]. Never sends Enter or writes into the same question twice.
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
        if !self.send_fence_known()? || seen.human_seq.is_none() {
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
        if let Some(stop) = self.first_key_wait(&first, seen, grace)? {
            return Ok(stop);
        }
        let (now, dialog) = match self.move_focus(steps, &first, seen, grace)? {
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
        let at = match self.question_text(&guard, text, &now, &key, priority)? {
            Keyed::Written(at) => at,
            Keyed::Not(p) => {
                // No answer came: the text may be in the field or on its way.
                // Its write is the loop's last, whose effect no read has shown
                // (R3a): the box read unchanged after the reconnect is the text
                // not taken, never written twice.
                if matches!(p, Pressing::Lost { .. }) {
                    self.question_pending(&now, QuestionFocus::FreeText, PendingKind::Text, false);
                }
                return Ok(p);
            }
        };
        self.question_pending(&now, QuestionFocus::FreeText, PendingKind::Text, false);
        // The worker's reaction, then the text read back.
        self.wait(
            &["seq", &at.to_string()],
            super::turn_end_loop::REACTION_WAIT,
        )?;
        // How long the screen had held still right before the read: the
        // settle's window when it latched.
        let mut still = latched_for(self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?, SETTLE_MS);
        let mut back = self.screen()?;
        let mut waited = false;
        loop {
            let reading = read_back(&back.rows, &first, text);
            match reading {
                Back::Exact => {
                    // Taken: the text's pending write has shown its effect
                    // (R3a), so a later read that shows the field empty again
                    // is the text gone, not a text never taken.
                    self.question.pending = None;
                    // A read-back the screen held still before is the read
                    // the Enter is decided on ([`Session::read_back`]).
                    self.read_back = still.map(|still| (back.clone(), still));
                    return Ok(Pressing::Done(Press::Typed { seq: back.seq }));
                }
                Back::Short | Back::Empty | Back::Other(_) if !waited => {
                    // Still drawing, not read yet, or read mid-redraw (a
                    // loaded machine's read can catch the dialog half drawn
                    // and read it as another): once more, after the screen
                    // held still. Only reads: nothing is written again.
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

    /// The person stamp the server reports now (`status`'s `human_ms=`):
    /// `Some(ms)` when a person has keyed the session; `None` when none ever
    /// has, from a host that does not stamp, or from a `status` that failed
    /// (the decision read's stamp then stands).
    fn person_stamp_now(&mut self) -> Result<Option<u64>, Fail> {
        let r = self.call(&["status"])?;
        Ok(if r.ok() {
            super::escalate::status_field(&r.stdout, "human_ms").and_then(|v| v.parse().ok())
        } else {
            None
        })
    }

    /// One question key, `key [if-human=<now's person count>] if-gen=<now's
    /// generation> if=<guard> <key>`: [`Keyed::Written`] with the server's
    /// seq, or why not: a skip (`reason=person`, a person keyed since the
    /// read: [`Press::Yielded`]; `reason=changed`: [`Press::Changed`]; no row
    /// matched: [`Press::Skipped`]), or a refusal ([`Self::refused_press`]).
    /// A key that may have been written latches the dialog's word. A read
    /// with no count sends the key with no person fence, so the recommended
    /// option of a dialog a text word fell back on stays answerable on such a
    /// host (the policy text is refused there instead, [`Self::question_text`]).
    fn question_key(
        &mut self,
        guard: &str,
        key: &str,
        now: &Screen,
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
        let args = match now.human_seq {
            Some(n) => format!("if-human={n} {args}"),
            None => args,
        };
        let args = self.acting(act, args);
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        if let Some(p) = self.refused_press(&r, &args)? {
            if matches!(p, Pressing::Lost { .. }) {
                // The key may have been written: the dialog's word is latched.
                self.latch_question_word();
            }
            return Ok(Keyed::Not(p));
        }
        Ok(self.written_or_not(&r, now.seq))
    }

    /// The policy text `text` of `priority`, written into the free-text row
    /// guarded by `guard` on `now` (`send if-human=<now's person count>
    /// if-gen=<now's generation> if=<guard> -- <text>`, one write, whose bytes
    /// carry no digit and no newline per `policy/question.rs`'s text rules),
    /// and remembered as the loop's own for question `key`
    /// ([`approval_loop::QuestionMemory::typed`]) whenever it may have been
    /// written: `OK`, a partial or failed write, no answer. A write the server
    /// did not make (`skipped`: a person keyed since the read,
    /// [`Press::Yielded`]; the screen moved, [`Press::Changed`]; no row,
    /// [`Press::Skipped`]; `ERR busy …`, `ERR halted`) is not remembered, so
    /// the loop may decide again. Never unfenced: a read with no person count
    /// (`"human_seq"`) or no generation writes nothing.
    fn question_text(
        &mut self,
        guard: &str,
        text: &str,
        now: &Screen,
        key: &str,
        priority: QuestionPriority,
    ) -> Result<Keyed, Fail> {
        let Some(generation) = now.generation.as_deref() else {
            return Ok(Keyed::Not(Pressing::Unconfirmed {
                why: "the read carried no screen generation to fence on".to_string(),
                retry: false,
            }));
        };
        let Some(count) = now.human_seq else {
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
                // Nothing written: decided again after the hold, the back-off.
                Pressing::Halted { .. } | Pressing::Refused { .. } => {}
                // In doubt — no answer, a partial or failed write, a usage
                // line the host was not meant to give: it may have written.
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

    /// A fenced write the server answered `OK …`: [`Keyed::Written`] with its
    /// seq (the dialog's word latched), or why it wrote nothing:
    /// `reason=person` ([`Press::Yielded`]), `reason=changed`
    /// ([`Press::Changed`]), no row matched ([`Press::Skipped`]).
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

    /// The latch ([`approval_loop::QuestionMemory::mode`]): the word of the
    /// decision in hand becomes the dialog's at the loop's first write on it.
    fn latch_question_word(&mut self) {
        if self.question.mode.is_none() {
            self.question.mode = self.question.deciding;
        }
    }

    /// Remember the question write just made on `now` with the focus at
    /// `focus` ([`approval_loop::QuestionPending`]), a focus move, an Enter,
    /// or the policy text (`kind`), `waited` when the screen already held
    /// still after it. A key written on the very box and focus a pending key
    /// of its kind was written on is a retry, counted in `retries` here when
    /// it goes, never when the loop decides to send it, so the next key the
    /// dialog does not show waits the press back-off (or, in `supervise`'s one
    /// look, is handed over). A policy text is never retried: its pending
    /// write is handed over when the box shows it not taken
    /// (`approval_loop.rs`, `decide_box`).
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

    /// Whether the server fences a press on the screen generation: `help
    /// key` asked once (and again after an outage forgot the caps). The same
    /// reply says whether `key` takes `act=` ([`Caps::act_word`]): no request
    /// of its own.
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

    /// Leading `act=<word>` for a dismissing key when [`Caps::act_word`] is supported.
    fn act_option(&self, act: Option<KeyAct>) -> Option<&'static str> {
        act.filter(|_| self.caps.act_word == Some(true))
            .map(KeyAct::option)
    }

    /// `args` with the dismissing key's `act=` in front, where the host
    /// takes it ([`Self::act_option`]).
    fn acting(&self, act: Option<KeyAct>, args: String) -> String {
        match self.act_option(act) {
            Some(option) => format!("{option} {args}"),
            None => args,
        }
    }

    /// One digit choosing an option, bound to the judged read's row,
    /// generation and person count under the server's write lock. Both an
    /// automatic policy choice and a controller's explicit answer use this:
    /// neither path proves that the caller is a manual human writer.
    ///
    /// Missing stamps or unsupported guards withhold the digit. A usage
    /// reply never drops a guard, and a person who keys in the gap yields
    /// the box. Abort, cancel, survey and decline keys have separate paths.
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

    /// Answer a focus-driven dialog ([`Choice::Focus`](super::super::policy::approval::Choice::Focus)):
    /// move focus `steps` options (each arrow fenced on `if-gen=` and `guard`,
    /// settled and re-read), verify the focused option still matches `label` (and
    /// [`aterm_phase::reset_row`] for usage-limit grant rows), then press `key`
    /// fenced on that read's generation and focused row. When `person` is `Some`,
    /// every key also carries `if-human=<n>`.
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
        // What only a host with the generation fence can do: never tried
        // again on this host.
        let cannot = |why: &str| {
            Ok(Pressing::Unconfirmed {
                why: why.to_string(),
                retry: false,
            })
        };
        // A move that did not come off: read and tried again at full power.
        let unconfirmed = |why: &str| {
            Ok(Pressing::Unconfirmed {
                why: why.to_string(),
                retry: true,
            })
        };
        // A fenced, confirmed move is safe at any distance within the box.
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
            // A person keyed since the read the key is fenced on: theirs.
            if r.skipped_person() {
                return Ok(Pressing::Done(Press::Yielded { seq: at }));
            }
            if r.skipped_changed() {
                return Ok(Pressing::Done(Press::Changed { seq: at }));
            }
            if r.skipped() {
                return Ok(Pressing::Done(Press::Skipped { seq: at }));
            }
            // The move drawn and settled, then read: a redraw arrives in
            // several writes (clear, then rows), so a read at the first change
            // can see a dialog half drawn and no focus. Both waits bounded,
            // never a sleep.
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
            // The same dialog, the focus where it was: the keys went nowhere
            // (a dialog drawn before its input is live takes none). Read and
            // decided again, like a screen that moved; a focus that moved
            // elsewhere is the manager's.
            if before.is_some() && focused_on(&now.rows) == before {
                return Ok(Pressing::Done(Press::Changed { seq: now.seq }));
            }
            return unconfirmed("the focus did not land on the option it was moved to");
        };
        // Re-check `reset_row`: another reset kind arriving while arrows moved
        // can change which row `reset_kinds` selects without moving focus off `label`.
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

    /// Execute [`super::super::policy::approval::Decision::Decline`]: step focus
    /// to `refusal`, press Tab to open the input, send `text`, and press Enter,
    /// one fenced keystroke per settled read ([`decline_step`]).
    ///
    /// Remembers the in-flight step in [`approval_loop::DeclinePending`] (up to
    /// [`MAX_DECLINE_UNSEEN_READS`] reads) so Tab or text is never sent twice,
    /// and yields if a person keyed within `grace`.
    pub(super) fn press_decline(
        &mut self,
        refusal: usize,
        text: &str,
        seen: &Screen,
        grace: Duration,
    ) -> Result<Pressing, Fail> {
        // What the decline cannot confirm is handed over, never tried again:
        // Tab toggles the input, so a keystroke is never written twice.
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
        let mut now = seen.clone();
        let mut written = 0;
        loop {
            let Some((q, _)) = box_on(&now.rows).filter(|(_, k)| *k == key) else {
                self.decline.pending = None;
                return Ok(Pressing::Done(Press::Skipped { seq: now.seq }));
            };
            let step = decline_step(&q, &now.rows, refusal, text);
            // One keystroke in flight: a read that decides the very keystroke
            // last written shows the box as it was keyed. Wait, never re-send.
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
            // Remembered before it goes: a keystroke whose answer never came
            // (a lost connection) may have been written, and is never written
            // again; one the server refused or skipped was not.
            self.decline.pending = Some(approval_loop::DeclinePending {
                key: key.clone(),
                step: step.clone(),
                unseen: 0,
            });
            // The person fence (R3b): a person who keyed since this read
            // skips the keystroke, `OK skipped reason=person`.
            let person = now.human_seq.map(|n| format!("if-human={n}"));
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
                // The Enter stays remembered: a read of the box still showing
                // the reason is the Enter not yet drawn, never one to send again.
                return Ok(Pressing::Done(Press::Pressed { seq: at }));
            }
            // Its effect drawn and settled, then read.
            self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
        }
    }

    /// Answer a Codex or Muse Code question via its notes field (`Choice::Note`):
    /// move focus to `None of the above`, press Tab to open the notes field, send
    /// `priority`'s fixed text via [`Self::question_text`], and press Enter once
    /// the field shows that text ([`note_step`]).
    ///
    /// Keeps at most one keystroke in flight (falling back via `fell_back` if Tab
    /// never opens the field), never sends Esc/Tab/digits into an open field, and
    /// requires `typed_person` to match `now.human_seq` when a short field shows
    /// only the tail of the text.
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
        // The keystroke last written, and the question as it was read when
        // that keystroke was decided.
        let mut last: Option<(NoteStep, NoteState)> = None;
        let mut unseen = 0;
        loop {
            let Some(q) = question_on(&now.rows).filter(|q| roles_question_key(q) == key) else {
                return Ok(Pressing::Done(Press::Changed { seq: now.seq }));
            };
            let state = note_state(&q, &now.rows);
            if let Some((step, was)) = &last {
                // A text still being drawn shows a piece of it: its head,
                // or, in a field too short for it, the rows at its cursor.
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
            // The tail alone is the loop's text only while no person has keyed
            // since the send, which was fenced on that same count: the field
            // read empty before it, so its hidden head is the loop's own.
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
                    self.question_text(guard, text, &now, &key, priority)?
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
            // Its effect drawn and settled, then read.
            self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
        }
    }

    /// A note's fenced key ([`Self::press_codex_note`]) as [`Keyed`]: refused
    /// ([`Self::refused_press`]; one whose answer never came latches the
    /// dialog's word, as it may have been written), skipped, or written.
    fn note_keyed(&mut self, r: &CtlReply, args: &str, seen: u64) -> Result<Keyed, Fail> {
        if let Some(p) = self.refused_press(r, args)? {
            if matches!(p, Pressing::Lost { .. }) {
                self.latch_question_word();
            }
            return Ok(Keyed::Not(p));
        }
        Ok(self.written_or_not(r, seen))
    }

    /// `key [if-human=<n>] if-gen=<generation> if=<guard> <key>`: the
    /// server's reply, and the arguments as sent (for a refusal's reason).
    /// The decline's keystrokes and the accounts probe's Enter
    /// ([`Self::probe_type`]) are pressed so: fenced three ways.
    pub(super) fn fenced_key(
        &mut self,
        guard: &str,
        key: &str,
        generation: &str,
        person: Option<&str>,
    ) -> Result<(CtlReply, String), Fail> {
        self.fenced_key_acting(guard, key, generation, person, None)
    }

    /// [`Self::fenced_key`] for a key that dismisses a box, carrying what it
    /// means (`act=`) where the host takes it ([`Self::act_option`]):
    /// `key [act=<w>] [if-human=<n>] if-gen=<generation> if=<guard> <key>`.
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

    /// A fenced press the server did not take, as the loop's [`Pressing`]
    /// (`None`: it answered `OK …`, pressed or skipped): not served, lost;
    /// `halted`; `busy`/`rate`; anything else, a usage line included, is
    /// [`Pressing::Unconfirmed`], since this path never retries without its
    /// fence.
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
            // Refused outright (a usage error): the same press would be
            // refused again.
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

    /// Press `key` under a row guard (`key [if-human=<n>] if=<guard> <key>`)
    /// without `if-gen=` (used where timers or refreshing panels repaint the
    /// screen under a stable guard row). Retries `ERR busy sink` up to
    /// [`BUSY_SINK_RETRIES`] times.
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

    /// Send `key backspace` to remove a stray digit from the composer, retrying
    /// `ERR busy sink` up to [`BUSY_SINK_RETRIES`] times and returning
    /// [`Backspaced::Refused`] on `halted`/`busy`/`rate` so the caller retains its
    /// pending-backspace state.
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
