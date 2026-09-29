// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The loop's keystrokes on a box: the approving `1`, under the guard the
//! approval policy returned ([`Session::press_one_guarded`]); the focus move
//! and Enter of an unnumbered dialog ([`Session::press_focused`]) and of a
//! question dialog's answer ([`Session::press_question`] — Enter only, never
//! a digit, never unfenced); a decline's focus move, Tab, text and Enter
//! ([`Session::press_decline`] — never a digit, never unfenced); and the
//! survey switch's `0` ([`Session::dismiss_survey`]).
//!
//! **What binds the `1` to the box that was judged.** The guard is the judged
//! row itself, anchored ([`super::super::policy::row_guard`]): the server
//! tests it against every visible row under the same terminal lock as the
//! write, so a box swapped between the read and the press — another command,
//! another path — matches nothing and the answer is `OK skipped` (audit
//! APR-6 measured the old guard, `Do.you.want.to.proceed`, approving a
//! swapped rm box). Where the server fences a press on the screen generation
//! (`key if-gen=<epoch>.<seq>`, read from `help key` once) and the judged read
//! carried one (`text --json`'s `"gen"`, [`Screen::generation`]), that
//! generation is sent too, and `OK skipped reason=changed` means the screen
//! moved: the loop reads and decides again. A screen that keeps moving under
//! the press (a blinking row, 2.1.281's auto-deny countdown) is handed to
//! the manager after [`MAX_CHANGED_PRESSES`] such skips — never pressed
//! with the fence dropped: the row guard alone names the box's first command
//! row, which a swapped box can show too. The content sequence is no fence
//! (`if-seq=`, the spelling this press used before lane C's review): it is
//! per grid and repeats after an alternate-screen re-entry, and the server
//! refuses it.
//!
//! **What the server can say instead.** `ERR halted …` ([`Pressing::Halted`])
//! parks the loop until the hold lifts; `ERR busy …` and `ERR rate`
//! ([`Pressing::Refused`], `busy sink` among them) back it off; in both the
//! box is read and decided again after, never pressed again as it was.

use super::super::policy::approval::{Answer, AnswerTarget, DeclineStep, decline_step};
use super::super::policy::question::{
    MAX_QUESTION_FOCUS_STEPS, focus_position, focus_row, option_label, target_focus,
};
use super::*;

/// The most keystrokes a decline takes: the focus moves across a permission
/// box's options to its refusal (three at most — the measured Bash box's
/// `Yes` to `No` is one), then Tab, the text and Enter.
const MAX_DECLINE_STEPS: u32 = 3 + 3;

/// How many fresh reads after a decline keystroke may still show the box as
/// it was keyed — its effect not drawn yet — before the box is handed over
/// ([`Session::press_decline`]). Each is a wait for the next change (at most
/// [`STRAY_SETTLE`]) and a settle: about ten seconds on a box that holds
/// still, less on one whose countdown ticks. The keystroke is never written
/// again: one not drawn may still be in flight.
const MAX_DECLINE_UNSEEN_READS: u32 = 5;

/// Why a decline is not carried out on a host that stamps no person's input.
const UNSTAMPED: &str = "this host does not stamp a person's input, and a decline's keystrokes \
                         could cross a person's";

/// What makes a box the SAME box for a decline ([`Session::press_decline`]):
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
        HumanInput::Never | HumanInput::Unknown => true,
    }
}

/// One fenced question key's reply, as the question press reads it.
enum Keyed {
    /// Written: the server's seq after it.
    Written(u64),
    /// Not written, and why, as the loop's answer.
    Not(Pressing),
}

impl<C: Ctl> Session<'_, C> {
    /// A question dialog's answer ([`Answer::FocusEnter`], the rule
    /// `answer-recommended@v1`): move the focus `steps` rows in the dialog's
    /// focus order, then press Enter on `target`, every key `key if-gen=<the
    /// generation of the read it was decided on> if=<the focused row as
    /// drawn> <up|down|enter>` — the check and the write under one server
    /// lock, so a key is written only while the dialog is the one read and
    /// the focus is where that read drew it.
    ///
    /// **What the fence does not cover** (the critique of 2026-09-25, R3): it
    /// proves the SCREEN did not change since the read, and nothing more. A
    /// key already written to the PTY that Claude Code has not read yet
    /// changes nothing on the screen, so the fence cannot see it. The loop
    /// covers those keys instead: its OWN by keying nothing while a read still
    /// shows the box as it was keyed (`approval_loop.rs`, the progression
    /// rule; below, one key in flight at a time), and a PERSON's by keying
    /// nothing until the session's person stamp says they have been quiet
    /// for `grace` (`human_grace_s`) — the stamp on the very read each key is
    /// fenced on (the loop's decision read, then each read inside this
    /// answer), and for a tab's first key, written after the wait below, a
    /// `status` read just before it; a person who keyed meanwhile gets
    /// [`Press::Yielded`], and nothing more is sent. The one round trip left
    /// — a person's key written between that read and the write, not yet
    /// read by Claude Code — is the SERVER's to close (R3b, built
    /// 2026-09-27): each key also names the read's person count (`key
    /// if-human=<its "human_seq">`), checked under the same lock as the
    /// screen fences, and a person who keyed since is `OK skipped
    /// reason=person`: [`Press::Yielded`], nothing written. A host that
    /// sends no count gets no person fence (the round trip stays, as
    /// before). The derived model `SupervisorQuestionAnswer` (aterm-spec
    /// `supervisor_question_answer_model`, Tier-1 bound in
    /// `run_engine_tests.rs`) proves the keys land where they were decided
    /// under exactly these guards — the person fence a mechanism of it
    /// (`PersonFence`) — and names the retry's premise, a key read by
    /// Claude Code before the screen held still 2 s after it, as the one
    /// assumption it rests on.
    ///
    /// * **Never unfenced.** A host that does not name `if-gen=` in `help
    ///   key`, or a read that carried no generation, is
    ///   [`Pressing::Unconfirmed`] (the box is handed over); the fallback of
    ///   [`Self::press_one_guarded`] (read, confirm, an unguarded key) is
    ///   never taken for a question.
    /// * **The first key waits** about a second after the tab first appears
    ///   ([`QUESTION_FIRST_KEY_IDLE`], once per question, O4): 2.1.282 refuses
    ///   keys inside a typeahead window after a dialog mounts. The person's
    ///   stamp is asked again after it (`status`'s `human_ms=`).
    /// * **One key in flight.** After each move the loop waits for the
    ///   content to change and to hold still, and reads; a move not seen yet
    ///   is waited for once more (`await idle 2000`). Still not seen, nothing
    ///   is re-sent: [`Press::Unseen`], and the loop's progression rule
    ///   decides on a later read (`approval_loop.rs`). A move that landed
    ///   anywhere but one row on, in its direction, or a dialog that is no
    ///   longer the same question, is [`Pressing::Unconfirmed`] or
    ///   [`Press::Changed`]: never an Enter on a row it did not choose.
    /// * **The Enter** goes only once a fresh read shows the focus on
    ///   `target` — option `n` with the label the decision read, the
    ///   multi-select button, or `1. Submit answers` — guarded on that row
    ///   as drawn (`❯ 1. …`, a multi-select checkbox included).
    /// * **Never** Esc, `n`, a digit, or a key on the free-text row, the chat
    ///   row or the review's cancel.
    ///
    /// Records the key it wrote last as the question's pending key
    /// ([`approval_loop::QuestionPending`]): the next read that shows the box
    /// as it was keyed is the loop's to wait on, never to key again at once.
    pub(super) fn press_question(
        &mut self,
        answer: &Answer,
        seen: &Screen,
        grace: Duration,
    ) -> Result<Pressing, Fail> {
        let Answer::FocusEnter { steps, target } = *answer;
        let unconfirmed = |why: &str| {
            Ok(Pressing::Unconfirmed {
                why: why.to_string(),
                retry: false,
            })
        };
        if steps.unsigned_abs() > MAX_QUESTION_FOCUS_STEPS {
            return unconfirmed("the focus is too far from the option to move it");
        }
        if self.caps.key_if == Some(false) || !self.gen_fence_known()? || seen.generation.is_none()
        {
            return unconfirmed("this host cannot fence a question's key on the screen generation");
        }
        let dialog_of =
            |rows: &[String]| aterm_phase::parse_prompt_v2(rows).and_then(|p| p.question_dialog);
        let Some(first) = dialog_of(&seen.rows) else {
            return Ok(Pressing::Done(Press::Changed { seq: seen.seq }));
        };
        let want = target_focus(target);
        let label = match target {
            AnswerTarget::Option(n) => option_label(&first, n).map(str::to_string),
            AnswerTarget::Button | AnswerTarget::ReviewSubmit => None,
        };
        // O4: the tab's first key waits for it to have been drawn, still,
        // about a second. The key after the wait is still fenced on the
        // generation decided on: a screen that moved meanwhile is skipped.
        let key_of = approval_loop::question_key(&first, &seen.rows);
        if self.question.keyed.as_deref() != Some(key_of.as_str()) {
            self.question.keyed = Some(key_of);
            self.wait(&["idle", QUESTION_FIRST_KEY_IDLE], QUESTION_FIRST_KEY_CAP)?;
            // The wait is time a person may have keyed in, which the
            // decision read's stamp cannot show: the server's, now.
            if self
                .person_stamp_now()?
                .is_some_and(|ms| !person_quiet(HumanInput::Ago(ms), grace))
            {
                return Ok(Pressing::Done(Press::Yielded { seq: seen.seq }));
            }
        }
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
            let at = match self.question_key(&guard, arrow, &now)? {
                Keyed::Written(at) => at,
                Keyed::Not(p) => return Ok(p),
            };
            // One key in flight: its effect seen before the next.
            self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
            let unmoved = |d: &Option<aterm_phase::QuestionDialog>| {
                d.as_ref()
                    .is_some_and(|d| d.same_question(&first) && d.focus() == at_focus)
            };
            let mut read = dialog_of(&now.rows);
            // Whether the screen HELD STILL after the key (the retry's
            // premise): only a latched wait says so.
            let mut settled = false;
            if unmoved(&read) {
                settled = matches!(self.wait(&["idle", IDLE_MS], WAIT_STEP)?, Wait::Latched);
                now = self.screen()?;
                read = dialog_of(&now.rows);
            }
            let Some(d) = read.filter(|d| d.same_question(&first)) else {
                // The dialog changed under the move (a person answered, the
                // tab moved on): read and decided again.
                return Ok(Pressing::Done(Press::Changed { seq: now.seq }));
            };
            if d.focus() == at_focus {
                // Not seen landing: the key may still be in flight. Nothing
                // is re-sent; the loop waits on it — at once when the screen
                // held still after it, else for it to.
                self.question_pending(&now, at_focus, false, settled);
                return Ok(Pressing::Done(Press::Unseen { seq: now.seq }));
            }
            if focus_position(&d, d.focus()) != Some(from + sign) {
                return unconfirmed("the focus did not land on the row it was moved to");
            }
            dialog = d;
            // The next key is fenced on this read: a person who keyed since
            // the decision read (its own stamp says so) gets the dialog —
            // nothing more is sent until they are quiet (R1).
            if !person_quiet(now.human, grace) {
                return Ok(Pressing::Done(Press::Yielded { seq: now.seq }));
            }
        }
        let same_label = match (&label, target) {
            (Some(l), AnswerTarget::Option(n)) => option_label(&dialog, n) == Some(l.as_str()),
            _ => true,
        };
        if dialog.focus() != want || !same_label {
            return unconfirmed("the focus did not land on the option it was moved to");
        }
        let Some(row) = focus_row(&dialog, want).filter(|&r| r < now.rows.len()) else {
            return unconfirmed("the focused row is not on the rows read");
        };
        let guard = super::super::policy::row_guard(&now.rows[row]);
        let at = match self.question_key(&guard, "enter", &now)? {
            Keyed::Written(at) => at,
            Keyed::Not(p) => return Ok(p),
        };
        self.question_pending(&now, want, true, false);
        Ok(Pressing::Done(Press::Pressed { seq: at }))
    }

    /// The person stamp the server reports NOW (`status`'s `human_ms=`):
    /// `Some(ms)` when a person has keyed the session; `None` when none ever
    /// has, from a host that does not stamp, or from a `status` that failed —
    /// the decision read's stamp then stands.
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
    /// seq, or why not — a skip (`reason=person`, a person keyed since the
    /// read: [`Press::Yielded`]; `reason=changed`: [`Press::Changed`]; no row
    /// matched: [`Press::Skipped`]), or a refusal ([`Self::refused_press`]).
    fn question_key(&mut self, guard: &str, key: &str, now: &Screen) -> Result<Keyed, Fail> {
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
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        if let Some(p) = self.refused_press(&r, &args)? {
            return Ok(Keyed::Not(p));
        }
        let at = r.seq().unwrap_or(now.seq);
        Ok(if r.skipped_person() {
            Keyed::Not(Pressing::Done(Press::Yielded { seq: at }))
        } else if r.skipped_changed() {
            Keyed::Not(Pressing::Done(Press::Changed { seq: at }))
        } else if r.skipped() {
            Keyed::Not(Pressing::Done(Press::Skipped { seq: at }))
        } else {
            Keyed::Written(at)
        })
    }

    /// Remember the question key just WRITTEN on `now` with the focus at
    /// `focus` ([`approval_loop::QuestionPending`]) — an Enter when `enter`,
    /// else a focus move — `waited` when the screen
    /// already held still after it. A key written on the very box and focus
    /// a pending key was written on IS a retry, and is counted in `retries`
    /// here — when it goes, never when the loop decides to send it — so the
    /// next key the dialog does not show waits the press back-off (or, in
    /// `supervise`'s one look, is handed over), and a press that sent
    /// nothing never counts as one.
    fn question_pending(
        &mut self,
        now: &Screen,
        focus: aterm_phase::QuestionFocus,
        enter: bool,
        waited: bool,
    ) {
        let identity = approval_loop::box_identity(&now.rows).unwrap_or_default();
        let retries = self
            .question
            .pending
            .as_ref()
            .filter(|p| p.identity == identity && p.focus == focus && p.enter == enter)
            .map_or(0, |p| p.retries.saturating_add(1));
        self.question.pending = Some(approval_loop::QuestionPending {
            identity,
            focus,
            enter,
            waited,
            unsettled: 0,
            retries,
            backed_off: false,
        });
    }

    /// Whether the server fences a press on the screen generation: `help
    /// key` asked once (and again after an outage forgot the caps).
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
        Ok(known)
    }

    /// Press `1` on the box `prompt` read at `seen`, under `guard` — the
    /// check and the press under one server lock where the server has `key
    /// if=` (`OK skipped seq=<n>`: no row matched, nothing written) — else
    /// read → confirm the same box is still up and the guarded row still
    /// drawn → press → wait for the worker to take it → re-read → backspace
    /// a digit that landed in the composer. The fallback presses nothing
    /// while the session survey is open on the confirming read
    /// ([`Pressing::Withheld`]): a `1` that reaches the survey instead of
    /// the box is a rating, and no digit is left to find.
    ///
    /// The capability probe is the reply: `OK …` proves the guard (pressed
    /// or skipped); a usage line or a bare `ERR` — what a build without
    /// `if=` answers, reading `if=…` as a key name — drops the generation
    /// fence first, then falls back for good; `halted`, `busy …` and `rate`
    /// are [`Pressing::Halted`] and [`Pressing::Refused`]; every other `ERR`
    /// (`no such session`, `badregex`) is an error, never an approval.
    ///
    /// A lost connection is [`Pressing::Lost`]: a press whose answer never
    /// came is not an approval, and is not sent again as it was — the loop
    /// rides the connection out, then reads and decides again
    /// ([`Self::drive`]). On the fallback path a `1` the server confirmed is
    /// written, lost connection or not: it is the approval it was, and what
    /// could not be checked after it — a digit in the composer — is checked
    /// on the next turn ([`Self::stray`]).
    pub(super) fn press_one_guarded(
        &mut self,
        prompt: &PromptV2,
        guard: &str,
        digit: &str,
        seen: &Screen,
    ) -> Result<Pressing, Fail> {
        if self.caps.key_if != Some(false) {
            // The fence needs both halves: a host that names it (`help key`,
            // asked once) and a read that carried the generation it names.
            let mut fence = if self.gen_fence_known()? {
                seen.generation.clone()
            } else {
                None
            };
            loop {
                let args = super::super::policy::key_args(guard, digit, fence.as_deref());
                let mut words: Vec<&str> = vec!["key"];
                words.extend(args.split(' '));
                let r = self.call(&words)?;
                if r.ok() {
                    self.caps.key_if = Some(true);
                    let seq = r.seq().unwrap_or(seen.seq);
                    return Ok(Pressing::Done(if r.skipped_changed() {
                        Press::Changed { seq }
                    } else if r.skipped() {
                        Press::Skipped { seq }
                    } else {
                        Press::Pressed { seq }
                    }));
                }
                let why = format!("key {args} failed: {}", r.stderr.trim());
                if self.unserved(&r) {
                    // The guard ran under the server's lock or not at all: a
                    // `1` it wrote went to the box, never the composer.
                    return Ok(Pressing::Lost { known: None, why });
                }
                if r.is_err("halted") {
                    return Ok(Pressing::Halted { why });
                }
                if r.is_err("busy") || r.is_err("rate") {
                    return Ok(Pressing::Refused { why });
                }
                if r.unknown_form() {
                    if fence.take().is_some() {
                        // The fence is what it did not know: the row guard
                        // alone, from now on.
                        self.caps.gen_fence = Some(false);
                        continue;
                    }
                    self.caps.key_if = Some(false);
                    break;
                }
                return Err(Fail::Hard(why));
            }
        }
        let now = self.screen()?;
        let reader = self.reader(&now.rows);
        let still = reader
            .prompt(&now.rows)
            .is_some_and(|q| q.command == prompt.command)
            && aterm_observe::row_matcher(guard)
                .is_ok_and(|m| now.rows.iter().any(|r| m.matches(r)));
        if !still {
            return Ok(Pressing::Done(Press::Skipped { seq: now.seq }));
        }
        // An unguarded `1` goes wherever keys go when it lands: to the box,
        // or — the box resolved first — to the session survey parked above
        // the composer, where it is the rating `Bad`, the human's to give, and
        // leaves no digit in the composer to find. With the survey open,
        // nothing is pressed: the box is the manager's.
        if reader.survey(&now.rows) {
            return Ok(Pressing::Withheld);
        }
        let r = self.call(&["key", digit])?;
        if !r.ok() {
            let why = format!("key {digit} failed: {}", r.stderr.trim());
            if self.unserved(&r) {
                self.stray = Some(prompt.command.clone());
                return Ok(Pressing::Lost { known: None, why });
            }
            if r.is_err("halted") {
                return Ok(Pressing::Halted { why });
            }
            if r.is_err("busy") || r.is_err("rate") {
                return Ok(Pressing::Refused { why });
            }
            return Err(Fail::Hard(why));
        }
        let seq = r.seq().unwrap_or(now.seq);
        let pressed = Press::Pressed { seq };
        // Let the worker take the press before looking for a stray digit: the
        // first content change after it, bounded.
        let after = match self
            .wait(&["seq", &seq.to_string()], STRAY_SETTLE)
            .and_then(|_| self.screen())
        {
            Ok(after) => after,
            Err(Fail::Lost(why)) => {
                self.stray = Some(prompt.command.clone());
                return Ok(Pressing::Lost {
                    known: Some(pressed),
                    why,
                });
            }
            Err(e) => return Err(e),
        };
        if stray_digit(self.reader(&after.rows), &after, digit) {
            let r = self.call(&["key", "backspace"])?;
            let skipped = Press::Skipped { seq: after.seq };
            if self.unserved(&r) {
                self.stray = Some(prompt.command.clone());
                return Ok(Pressing::Lost {
                    known: Some(skipped),
                    why: format!("key backspace failed: {}", r.stderr.trim()),
                });
            }
            return Ok(Pressing::Done(skipped));
        }
        Ok(Pressing::Done(pressed))
    }

    /// The choice on a dialog answered by its FOCUS (the folder-trust
    /// dialog, unnumbered; under full power any unnumbered box — the `Tool
    /// use` box whose vendor default is `No`, Codex's folder gate; the
    /// usage-limit dialog, whose wait row is reached with the arrows and
    /// confirmed with Enter, never by a digit —
    /// [`Choice::Focus`](super::super::policy::approval::Choice::Focus)):
    /// move the focus `steps` options — each arrow fenced on the generation
    /// of the read that showed the dialog as judged (`key if-gen=<g>
    /// if=<the judged row> down`), the screen read again once it has moved
    /// and held still (`await seq`, then `await idle`) — then,
    /// once a fresh read by the session's reader shows the SAME dialog
    /// (kind, and its folder, subject or wait row) with the focus on the
    /// option labelled `label`, press
    /// `key` — Enter, or the key a Codex effort box applies the focused
    /// effort to this conversation with (`s`) — fenced on THAT read's
    /// generation and guarded on the focused row
    /// itself, `❯` and all: the Enter lands only while the focus is where
    /// the read saw it. Never without the generation fence (a host or a read
    /// without it: [`Pressing::Unconfirmed`]), never by the fallback's
    /// unguarded keys, and never Esc (the trust dialog's cancel EXITS Claude
    /// Code; the usage-limit dialog's cancels the wait). A focus that did not
    /// land where it was sent, or a dialog that
    /// changed under the moves, is [`Pressing::Unconfirmed`]: the box is the
    /// manager's under the safe rules, and at full power read and tried again
    /// ([`Session::press_missed`]). A move of any distance within the box's
    /// options goes: each arrow is fenced and confirmed.
    pub(super) fn press_focused(
        &mut self,
        prompt: &PromptV2,
        steps: i32,
        label: &str,
        key: &str,
        guard: &str,
        seen: &Screen,
    ) -> Result<Pressing, Fail> {
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
        if self.caps.key_if == Some(false) || !self.gen_fence_known()? {
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
            let args = super::super::policy::key_args(guard, arrow, Some(&generation));
            let mut words: Vec<&str> = vec!["key"];
            words.extend(args.split(' '));
            let r = self.call(&words)?;
            if let Some(p) = self.refused_press(&r, &args)? {
                return Ok(p);
            }
            let at = r.seq().unwrap_or(now.seq);
            if r.skipped_changed() {
                return Ok(Pressing::Done(Press::Changed { seq: at }));
            }
            if r.skipped() {
                return Ok(Pressing::Done(Press::Skipped { seq: at }));
            }
            // The move drawn and SETTLED, then read — a redraw arrives in
            // several writes (clear, then rows), and a read at the first
            // change saw a dialog half drawn and no focus (measured on this
            // lane's live check). Both waits bounded, never a sleep.
            self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
        }
        let reading = aterm_phase::read(self.program.as_deref(), &now.rows, None);
        let focused = reading.prompt.as_ref().and_then(|q| {
            (q.kind == prompt.kind && q.command == prompt.command)
                .then(|| q.focused())
                .flatten()
        });
        let Some(focused) = focused.filter(|o| o.label == label) else {
            // The same dialog, the focus where it was: the keys went nowhere
            // — a dialog drawn before its input is live takes none (measured
            // live 2026-09-24: an arrow 0.3 s after Claude Code 2.1.281 drew
            // the trust dialog was dropped). Read and decided again, like a
            // screen that moved; a focus that moved ELSEWHERE is the
            // manager's.
            if before.is_some() && focused_on(&now.rows) == before {
                return Ok(Pressing::Done(Press::Changed { seq: now.seq }));
            }
            return unconfirmed("the focus did not land on the option it was moved to");
        };
        let Some(generation) = now.generation.clone() else {
            return unconfirmed("the read carried no screen generation to fence on");
        };
        let enter_guard = super::super::policy::row_guard(&now.rows[focused.row]);
        let args = super::super::policy::key_args(&enter_guard, key, Some(&generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        if let Some(p) = self.refused_press(&r, &args)? {
            return Ok(p);
        }
        let at = r.seq().unwrap_or(now.seq);
        Ok(Pressing::Done(if r.skipped_changed() {
            Press::Changed { seq: at }
        } else if r.skipped() {
            Press::Skipped { seq: at }
        } else {
            Press::Pressed { seq: at }
        }))
    }

    /// THE DECLINE ([`super::super::policy::approval::Decision::Decline`]):
    /// the box's refusal `refusal` amended with `text`, one keystroke per
    /// fresh read of the SAME box ([`decline_box`]: its kind, whether its
    /// head is off the screen, its command, and the vendor note at its foot
    /// — a box taller than the screen has no command to tell it by, and its
    /// text quotes that note), each chosen by [`decline_step`] from the state
    /// that read shows, fenced on that read's generation and guarded on the
    /// row that shows the state — `key … down|up` from the focused row, `key
    /// … tab` on the focused `No`, `send … -- <text>` on the open, empty
    /// input, `key … enter` on the input that shows exactly `text` — the
    /// screen settled (`await seq`, then `await idle`) and read again after
    /// each. Never the digit (on the refusal it is the bare `No`, which stops
    /// the worker for a person), and never without the generation fence on
    /// `key` AND `send` (a host or a read without it:
    /// [`Pressing::Unconfirmed`]).
    ///
    /// **ONE KEYSTROKE IN FLIGHT, NEVER WRITTEN TWICE** (the review of
    /// 2026-09-25, F1). The fence proves only that the SCREEN did not change
    /// since the read; a keystroke written and not yet read by Claude Code
    /// changes nothing on it. Tab TOGGLES the input (measured, 2.1.282), so a
    /// second Tab behind a first not yet drawn shuts the input again, and the
    /// text sent after it meets a closed Select — whose digit (`$1` of a
    /// quoted `rm -rf $S/$1`) chooses an option: `1` is `1. Yes`, on a box
    /// nobody could read. So a keystroke written is remembered
    /// ([`approval_loop::DeclinePending`], across calls): a read of the same
    /// box that decides that very keystroke again shows the box as it was
    /// keyed, and nothing is written — the screen is waited on and read
    /// again, up to [`MAX_DECLINE_UNSEEN_READS`] times, and then the box is
    /// handed over ([`Pressing::Unconfirmed`]) with the keystroke never
    /// re-sent. The loop decides a box only once it has shown
    /// [`BOX_SETTLE`] (Claude Code refuses keys in a window after a dialog
    /// mounts, O4), so a dropped first keystroke is not what the retry would
    /// have been for.
    ///
    /// **A PERSON KEYING THE SESSION GETS IT** (F3; R1 of the question
    /// answer): no keystroke is written unless the person stamp on the read
    /// it is fenced on says
    /// no person keyed the session within `grace` (`[harness] human_grace_s`)
    /// ([`Press::Yielded`]: the loop waits for their quiet and decides
    /// again). A host that stamps no person's input cannot say, and the box
    /// is handed over. The derived model `SupervisorDeclineKeys` (aterm-spec
    /// `supervisor_decline_keys_model`, Tier-1 bound in
    /// `run_engine_tests.rs`) proves that under these two guards every
    /// keystroke meets the box as the read it was decided on showed it.
    ///
    /// A box gone from a read, or another box there, is [`Press::Skipped`];
    /// a fenced keystroke the screen moved under is [`Press::Changed`], and
    /// the loop decides again from a fresh read — the decision reads a
    /// decline under way as the same decline
    /// ([`super::super::policy::approval::decline`]), so it goes on where it
    /// stopped. A state no keystroke answers (text in the input that is not
    /// `text`, the refusal gone), or more keystrokes than
    /// [`MAX_DECLINE_STEPS`], is [`Pressing::Unconfirmed`]: what was written
    /// before it never chose an option — an arrow moved the focus, and Tab
    /// and the text opened and filled the refusal's input, which only its
    /// Enter submits. [`Press::Pressed`] is that Enter, landed.
    pub(super) fn press_decline(
        &mut self,
        refusal: usize,
        text: &str,
        seen: &Screen,
        grace: Duration,
    ) -> Result<Pressing, Fail> {
        // What the decline cannot confirm is handed over, never tried again:
        // Tab TOGGLES the input, so a keystroke is never written twice.
        let unconfirmed = |why: &str| {
            Ok(Pressing::Unconfirmed {
                why: why.to_string(),
                retry: false,
            })
        };
        if self.caps.key_if == Some(false)
            || !self.gen_fence_known()?
            || !self.send_fence_known()?
        {
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
            // Remembered BEFORE it goes: a keystroke whose answer never came
            // (a lost connection) may have been written, and is never
            // written again; one the server refused or skipped was not.
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
                DeclineStep::Submit { guard } => {
                    self.fenced_key(guard, "enter", &generation, person.as_deref())?
                }
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

    /// `key [if-human=<n>] if-gen=<generation> if=<guard> <key>`: the
    /// server's reply, and the arguments as sent (for a refusal's reason).
    fn fenced_key(
        &mut self,
        guard: &str,
        key: &str,
        generation: &str,
        person: Option<&str>,
    ) -> Result<(CtlReply, String), Fail> {
        let args = super::super::policy::key_args(guard, key, Some(generation));
        let args = match person {
            Some(fence) => format!("{fence} {args}"),
            None => args,
        };
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        Ok((r, args))
    }

    /// A fenced press the server did not take, as the loop's [`Pressing`]
    /// (`None`: it answered `OK …`, pressed or skipped): not served — lost;
    /// `halted`; `busy`/`rate`; anything else, a usage line included, is
    /// [`Pressing::Unconfirmed`] — the press this path makes is never
    /// retried without its fence.
    pub(super) fn refused_press(
        &mut self,
        r: &CtlReply,
        args: &str,
    ) -> Result<Option<Pressing>, Fail> {
        if r.ok() {
            self.caps.key_if = Some(true);
            return Ok(None);
        }
        let why = format!("key {args} failed: {}", r.stderr.trim());
        Ok(Some(if self.unserved(r) {
            Pressing::Lost { known: None, why }
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

    /// the survey switch's press: `key if=^●.How.is.Claude.doing 0`, the
    /// check and the press under one server lock, as the approval press
    /// guards its `1` — `OK skipped seq=<n>` means no row matched and nothing
    /// was written, so a survey that left first never gets a `0` in the
    /// composer (a copy quoted in the transcript does not match:
    /// [`survey_row`]).
    /// `0` is the only key it ever presses. A host without the guard (a usage
    /// line or a bare `ERR`) is never pressed unguarded: [`Dismiss::Unguarded`],
    /// and the survey is said instead. `ERR busy sink` is retried
    /// [`BUSY_SINK_RETRIES`] times (the guard is the survey's own row, so a
    /// retry cannot land anywhere else); `ERR halted`, a busy sink that does
    /// not clear, `ERR busy …` and `ERR rate` are [`Dismiss::Refused`] — the
    /// loop parks or backs off and tries the survey again; a request not
    /// served is ridden out; any other `ERR` ends the loop. `seen` is the seq
    /// of the screen read before.
    pub(super) fn dismiss_survey(&mut self, seen: u64) -> Result<Dismiss, Fail> {
        if self.caps.key_if == Some(false) {
            return Ok(Dismiss::Unguarded);
        }
        let cond = format!("if={}", survey_row());
        let mut busy = 0;
        loop {
            let r = self.call(&["key", &cond, "0"])?;
            if r.ok() {
                self.caps.key_if = Some(true);
                return Ok(if r.skipped() {
                    Dismiss::Skipped
                } else {
                    Dismiss::Pressed {
                        seq: r.seq().unwrap_or(seen),
                    }
                });
            }
            let why = format!("key {cond} 0 failed: {}", r.stderr.trim());
            if self.unserved(&r) {
                return Err(Fail::Lost(why));
            }
            if r.unknown_form() {
                self.caps.key_if = Some(false);
                return Ok(Dismiss::Unguarded);
            }
            if r.is_err("busy sink") && busy < BUSY_SINK_RETRIES {
                busy += 1;
                std::thread::sleep(BUSY_SINK_BACKOFF);
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
}
