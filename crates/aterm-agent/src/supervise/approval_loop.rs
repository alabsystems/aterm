// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The approval policy in the loop: every box a look finds is decided by
//! [`decide`] — the ONE decider, under the owner's `[harness] approve`
//! level (full power by default) — and an approval is pressed under the
//! guard it returns ([`Session::press_one_guarded`], or, for a box answered
//! by its focus — an unnumbered box, the usage-limit dialog's wait row — the
//! confirmed focus move of [`Session::press_focused`]). A DECLINE
//! ([`Decision::Decline`]) is carried out keystroke by keystroke from fresh
//! reads ([`Session::press_decline`]: one keystroke in flight, never written
//! twice, and none while a person keys the session; the derived model
//! `SupervisorDeclineKeys`) and, like an approval, is an answer: noted,
//! ledgered `declined` with the text typed, journaled `DECLINED seq=<n>
//! rule=<id> <subject> => <text>` — no badge, no mail, nobody waited on —
//! and the loop looks again once the box has left. The same box back after
//! [`REASKED_AFTER`] declines since the last review point is the worker not
//! acting on its reason yet: declined again after [`reask_pause`], as a
//! re-asked question is answered again, never handed over for the count.
//!
//! What the loop adds around the pure decision is only what the pure
//! decision cannot know, read FRESH for every box it decides: the session's
//! foreground PROGRAM (`status program=`, lane C: the box is read by that
//! program's reader, so a shell showing a captured box is never judged by
//! Claude Code's grammar), its working directory (`meta cwd=`, for a box
//! that needs it — the rm circuit breaker and the folder-trust dialog; the
//! launch directory, which a trust dialog asks about), the permission mode
//! its footer showed on an earlier read (a 2.1.280 box replaces the footer),
//! the owner's home, uid and `$TMPDIR`, and the level, roots and question
//! switch [`SuperviseOpts::policy`] sets (`approve`, `answer_questions`).
//! Nothing is read under `approve = "none"` but for a question, which no
//! level limits: every other box escalates. Every
//! decision is a row of the approval ledger ([`approvals`]); a press no safe
//! rule proved carries `unproven: <why the safe rules did not prove it>` as
//! that row's reason, in the `--notes` line, and in a journal-only
//! `UNPROVEN` line.
//!
//! **The repeat cap** is the safe rules': the same command approved
//! [`MAX_APPROVALS_OF_ONE_COMMAND`] times since the last review point is
//! handed over on its next return under `approve = "safe"`. Under full power
//! a box that comes back is answered again — the worker asks, and nobody
//! else is there to (a press the box never took is caught below, not by a
//! count). A question is never counted by that cap: every question's parser
//! "command" is empty, so every tab of a dialog would be "the same prompt".
//!
//! **What an approval names.** The ledger row, the `--notes` line, the
//! `APPROVED` line and the escalation reason name the decision's SUBJECT
//! ([`shown_subject`]: the judged command, path or folder — a trust
//! dialog's backstop warnings, an rm breaker's resolved targets, the option
//! full power chose), one line; the parser's command only when a box has no
//! decision to name it.
//!
//! **The box must leave.** After a press the loop waits for the content to
//! move past it; a move that leaves the SAME box on the screen
//! ([`box_identity`]: its auto-deny countdown ticked or wiped, a blank row
//! gone, the transcript above it reflowed) is no move, and the wait goes on
//! — so a `1` the box never took is "the box did not change after the
//! press", not an approval pressed again each second, and a box the vendor
//! redraws before it takes it down is not pressed twice (measured live,
//! 2.1.283: every approved rm box was). What a box's rows ARE — its span,
//! less the countdown, the focus marker and the blank rows — is read in one
//! place, [`box_rows`], which the loop's review key of a box reads too
//! (`run.rs`'s `box_key`): the press and the escalation agree on what "the
//! same box" is.
//!
//! Two things hold a box before it is judged or pressed. The vendor's key
//! guard: a box is judged only on a read made once it has shown
//! the setup's box settle since the loop FIRST read it ([`BOX_SETTLE`]) —
//! a younger one is waited on (`await seq`, bounded by what is left) and
//! read again, so the press is fenced on a fresh read, never on one a sleep
//! has made stale. And a person: an answer the policy has is held for
//! `human_grace_s` after their last keystroke ([`Session::held_for_person`]),
//! but for THE LAUNCH ([`Session::launch_call`]): a folder-trust dialog drawn
//! by the `claude` or `codex` the person's Enter started at an integrated
//! prompt, which the server's launch witness shows they have not keyed since
//! (aterm-gui's `launch_witness` carries the proof).
//!
//! **The usage-limit dialog's grant row** (`limit-reset@v1`, under `[harness]
//! limit_reset`, which ships `false`; `reset_loop.rs`,
//! `docs/SPEC-claude-accounts-resets-2026-10-02.md`). The policy decides the
//! row in front of the wait row; before its first key the loop's own gates
//! and then its HOST are asked ([`Session::reset_take`]), and anything but
//! the host's go answers the dialog with the decision the same read gets
//! WITHOUT that rule ([`Session::reset_fallback`], through
//! [`Session::auto_act`] again) — the wait row, by the requests a build with
//! the key off sends. The row's press is the confirmed focus move every
//! focus-answered box gets, its every key fenced on the judged read's person
//! count as well (the host is asked between that read and the first key);
//! once its Enter is written, what it opens is the
//! loop's own step, owed and run from there ([`Session::reset_run`]), never
//! another rule's box. A press that does not land is tried again on the
//! press back-off and never badged ([`Session::reset_press_missed`]); one
//! that cannot be made at all is passed over, never handed to a person. Its
//! approval is named by the row's label, so the safe rules' repeat cap never
//! counts it against the wait row of the same dialog.
//!
//! **A question dialog** (`policy/question.rs`: under `[harness]
//! answer_questions`, Enter on its recommended option —
//! `answer-recommended@v1` — or, under the priority words `long-term` /
//! `short-term`, that word's policy text written into its free-text row and
//! Enter on it — `answer-long-term@v1`, `answer-short-term@v1`: THE POLICY
//! TEXT, below) is keyed only when the things the pure decision cannot know
//! hold, each read fresh — else the loop waits
//! and decides again on a fresh read, and never escalates for the wait
//! ([`Session::question_hold`]):
//!
//! 1. **A person has been quiet** (R1): the read's person stamp (`text
//!    --json`'s `"human_ms"`, the server's per-session record of a person's
//!    last gesture, [`HumanInput`]) is at least `human_grace_s` old, or no
//!    person has keyed the session. This is what covers a person's keys
//!    still in flight: the generation fence proves only that the SCREEN did
//!    not change, and a key written to the PTY that Claude Code has not read
//!    yet changes nothing on it. It covers the person evidence the decision
//!    cannot see, too (R2 (a) and (b): a focus a person moved, the review a
//!    person skipped to): answered only after their quiet. A host that sends
//!    no stamp gets the stand-in: the screen holding still
//!    [`QUESTION_STANDIN_QUIET`], once per box. The press asks again before
//!    every later key of the answer — each read inside it carries its own
//!    stamp, and `status` is asked after the first-key wait — and a person
//!    who keyed meanwhile ([`Press::Yielded`]) is waited for here in turn.
//! 2. **The loop's own last key took effect** (R3a): no further key while a
//!    read still shows the box as it was keyed — the same [`box_identity`]
//!    and the same focus. On such a read the loop waits for `await idle
//!    2000` and reads again; still unchanged, and only once the screen HELD
//!    STILL (the wait latched), the key is sent once more (a key the dialog
//!    refused inside its typeahead window is gone, O4) — counted as sent
//!    when it is written, not when it is decided. A key still in flight when
//!    a retry is written would answer the next tab with its vendor focus —
//!    option 1, where the tool puts its recommendation, or the review's
//!    `Submit answers`, never its cancel: the one residual, and why every
//!    retry waits for the screen to hold still first. Unchanged after that
//!    retry, the key is tried again for as long as the dialog stands, each
//!    try on the press back-off ([`Session::box_back_off`]: a pause that
//!    doubles from [`REFUSAL_PAUSE`] to a minute, the session badged — as
//!    information, the tries going on — once it has missed for
//!    [`PRESS_BADGE_AFTER`]); a screen that never holds still is waited on
//!    the same back-off, and never keyed until it does. Nobody is there to
//!    hand it to. `supervise`'s
//!    one look hands the box to the manager it returns to instead: after the
//!    one retry, or after [`MAX_UNSETTLED_WAITS`] waits that ran out.
//! 3. **Every check on a multi-select is the loop's own** (R2 (d)): it
//!    remembers the options it toggled, per question; a checked option it
//!    did not toggle — a recommended one included — or one it toggled that
//!    reads unchecked again is a person's answer begun, and handed over.
//! 4. **A re-asked question is answered again** (O5): the same question
//!    answered [`REASKED_AFTER`] times since the last review point is the
//!    model re-asking it, and nobody else is there to answer it — it is
//!    answered again after a pause that doubles with each further return,
//!    from [`REFUSAL_PAUSE`] to a minute ([`reask_pause`]), journaled
//!    `WAITING … re-asking`. No count hands it over. Under a text word the
//!    pause comes before ANY key of the answer — its moves, its policy text,
//!    a falling-back tab's toggle — so the dialog waits it out with nothing
//!    of the loop's in it; under `recommended` before the answering Enter's
//!    keys, as on main.
//!
//! **Autopilot off is hands off here too.** A box decided where the
//! session's `autopilot` word reads `off` — the bare `meta` its decision
//! made, or one more on a host that keeps the word
//! ([`Session::autopilot_read`]) — is neither pressed nor escalated, and the
//! loop parks until the word changes ([`Session::park_while_off`]). The
//! harness's own loops alone (`watch`, the hosted loop): a manager's one
//! look (`supervise`) is the manager's.
//!
//! **HOW it is answered is resolved per dialog** (`decide_box`): the
//! session's `questions` word over the global `answer_questions` and
//! `question_priority` ([`Session::question_policy`], the one resolver the
//! turn end's answer in prose uses too — for its PRIORITY words only: the
//! session's `ask` holds its dialogs, never a question it asks in prose,
//! which no `aterm drive answer` can answer and no hand-back watches). The
//! word at the loop's FIRST key or text on a dialog is LATCHED
//! ([`QuestionMemory::mode`]): a dialog whose word reads otherwise later is
//! handed over (the word changed mid-dialog: decision 13), while before the
//! first key a new word simply applies. The latch is THIS supervisor's
//! memory, so it covers the session's own word: a change to the global
//! `question_priority` (to any `[harness]` key) restarts the window's
//! supervisor (`harness_host`), whose fresh memory latches nothing — the
//! new word answers the rest of a dialog the old one only moved on or
//! partly answered, and one it typed into reads as a person's (its text
//! remembered by no one: THE POLICY TEXT, below) and is handed over. A text
//! word on a window that cannot fence a typed answer — `send` without its
//! generation fence, a read with no person count (`human_seq`: no
//! `if-human=`), no person stamp — answers the WHOLE dialog with its
//! recommended option instead, ledgered under the word's own rule with
//! `(recommended: this window cannot fence a typed answer: …)` in its
//! subject (decision 12: never parked). Under `recommended` — the default —
//! every dialog's decision, key and request is main's, byte for byte: its
//! keys carry main's `if-human=` wherever the read has a count; no `help
//! send` asked, nothing written into the free-text row. (A question
//! asked in PROSE is main's in decision, key and text, and reads the
//! session's word — one bare `meta` more than main — before its act:
//! `Session::decide_to_act`.)
//!
//! **THE POLICY TEXT** — the one place a supervisor key lands in a
//! question's free-text row — rests on what the loop adds to the decider:
//!
//! * **Memory.** The loop remembers, per question, the priority whose text a
//!   `send` of its own may have written there ([`QuestionMemory::typed`]:
//!   recorded on `OK`, on a partial or failed write and on no answer; never
//!   on a skip or `ERR busy …`, which wrote nothing). Text that reads as the
//!   policy's with no such memory — a person's, or the loop's own after a
//!   restart — is a person's, and handed over: a lost memory is a stall,
//!   never an answer (the model's `OwnTextRemembered`). The memory is the
//!   dialog's: it goes with the dialog, at the worker's next busy read or at
//!   a look that reads the worker at its composer with no dialog up, so the
//!   same question asked again in a new dialog is written afresh
//!   (ATERM-KI-1015-1 F4); a frame that reads neither keeps it.
//! * **The exact read-back.** After the one `send` the field must read the
//!   text back byte for byte on the reader's one-space join (ticked, on a
//!   multi-select) before the press returns [`Press::Typed`]; the Enter is a
//!   later decision, on a read that shows the field holding it, and goes only
//!   while the field's text and tick are still what that read showed. A
//!   read-back the screen held still for before it (its settle latched) IS
//!   that read ([`Step::Decide`]: the Enter costs the decision's `status`
//!   and `meta` and the key, fenced on the read-back's generation); one it
//!   did not gets a fresh look.
//! * **The person fence at the write.** The `send`, and every key, names the
//!   person count of the read it was decided on (`if-human=`, main's R3b
//!   fence); not atomic with the person's own write.
//! * **Never written twice.** A text whose box never showed it is handed
//!   over, not re-sent — attended or not: main's P3 tries a KEY the dialog
//!   did not take again after the press back-off, but a text sent again
//!   behind one still unread would land in a field no longer empty, and no
//!   other key may follow it on that tab (the model's `HarnessType` has no
//!   retry, `TextNeverWrittenTwice`) — and so is a field the loop remembers
//!   writing that reads empty again.
//! * **Never an Enter in a multi-select's field** (c3c on 2.1.283: it
//!   unticks the row and the dialog answers "did not answer"): the focus is
//!   moved off it onto `Next`/`Submit` first, and the Enter goes there.
//!
//! A person's input after the loop's text (R1) makes the dialog's end theirs:
//! the loop may still press Enter on its own text once they are quiet, the
//! field intact, but the dialog is not told as the policy's choice — whether
//! the read the loop decides on shows them or the `status` made for that
//! decision does (`QuestionMemory::person_gave_input`).
//!
//! The press itself ([`Session::press_question`], and for the policy text
//! [`Session::press_question_text`]) waits about a second after a tab first
//! appears before its first key or text (O4), moves the focus one row at a
//! time — each move seen landing before the next, never re-sent — and
//! presses Enter guarded on the focused row as drawn, every key and the
//! text's one `send` under the generation fence. A fenced key skipped because the screen moved
//! (`reason=changed`) settles and decides again, never presses again at
//! once. Every question decision, answered or escalated, journals the
//! dialog's rows (`DIALOG seq=<n> rule=<id> <rows>`, O3).
//!
//! The derived model `SupervisorQuestionAnswer` (aterm-spec
//! `supervisor_question_answer_model`; the policy text's twin is
//! `SupervisorQuestionText`) states 1 and 2 as guards over keys
//! split into their write and Claude Code's read of them, and proves that
//! every key meets the dialog as the read it was decided on showed it —
//! never the free-text field, a next tab, the review's cancel or the
//! composer — naming the one round trip between a stamp's read and the
//! write, and the retry's premise (a key is read by Claude Code before the
//! screen holds still 2 s after it), as the assumptions it rests on. At
//! `Text = 1` it adds the policy text, each tab on its own route — on a
//! tab the text goes to, an Enter only over the field holding exactly the
//! loop's own text and the text only into its tab's empty focused field,
//! once; on a tab that falls back, the recommended path's keys — and the
//! memory as a third assumption.
//! `tier1_a_question_key_goes_only_where_the_model_allows`
//! (`run_engine_tests.rs`) binds this loop to it, and
//! `tier1_the_policy_text_goes_only_where_the_model_allows` and
//! `tier1_a_tab_that_falls_back_goes_only_where_the_model_allows`
//! (`run_question_text_tests.rs`) bind its text path at `Text = 1`, with a
//! blind loop (memory seeded over a person's text) as the negative control.

use super::super::policy::WorkerEnv;
use super::super::policy::approval::RULE_RM_BREAKER_WARN;
use super::super::policy::approval::{
    AnswerTarget, ApprovalCtx, Choice, DeclineStep, QuestionPriority, RULE_AUTOCOMPACT_OFFER,
    RULE_LIMIT_RESET, RULE_LIMIT_WAIT, RULE_READ_ONLY, RULE_TRUST_ANY, RULE_TRUST_DIALOG, decide,
    proof_would_lift, unvoiced_tools,
};
use super::super::policy::box_call;
use super::super::policy::box_facts::{BoxFacts, QuestionFacts};
use super::super::policy::question::{begun, fall_back_to, is_question_rule};
use super::*;
use crate::harness::upgrade_drive::{LiveTab, roster_rows, unique_tab_for_group};
use aterm_phase::{QuestionDialog, QuestionFocus, QuestionForm};

#[cfg(test)]
thread_local! {
    /// A test's planted panic in the approval policy (this thread's): the
    /// loop panics where it has just decided a box, before the decision's
    /// mark — the bug a recorded run reproduces on its replay, at the same
    /// door (`supervise::tape::replay`'s PANICKED).
    pub(crate) static PANIC_AT_DECIDE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// One answer since the last review point: the parser's command (the safe
/// rules' cap, [`MAX_APPROVALS_OF_ONE_COMMAND`]); for a key that answers a
/// question (a single-select or preview option, the multi-select button, the
/// review's submit; not a toggle) the question it answered ([`question_key`],
/// for [`REASKED_AFTER`]); and for a decline, the box it declined
/// (`press.rs`'s `decline_box`), counted apart from approvals.
#[derive(Debug, Clone)]
pub(super) struct Approved {
    command: String,
    question: Option<String>,
    declined: Option<String>,
}

/// What the loop does with a box it answers.
enum Act {
    /// Press the approving option under `guard` ([`Decision::Approve`]).
    Approve { guard: String, choice: Choice },
    /// Refuse it amended with `text` ([`Decision::Decline`]).
    Decline { refusal: usize, text: String },
}

/// How often the same question is answered, or the same box declined, since
/// the last review point before its next return is the worker asking again
/// (O5): answered again, after [`reask_pause`].
const REASKED_AFTER: usize = 2;

/// The pause before a re-asked question answered `answered` times is
/// answered again: [`REFUSAL_PAUSE`] at the first re-ask, twice the last at
/// each next, at most [`PRESS_RETRY_MAX`].
pub(super) fn reask_pause(answered: usize) -> Duration {
    let n = u32::try_from(answered.saturating_sub(REASKED_AFTER)).unwrap_or(u32::MAX);
    crate::supervise::ladder::doubling(REFUSAL_PAUSE, n, PRESS_RETRY_MAX)
}

/// How many waits for the screen to hold still (`await idle 2000`, a
/// [`WAIT_STEP`] each) a key whose effect no read has shown gets before the
/// wait is backed off ([`Session::box_back_off`]), or, in `supervise`'s one
/// look, the box handed to its manager. The retry needs a screen that held
/// still after the key, so one that never does is never keyed again.
const MAX_UNSETTLED_WAITS: u32 = 3;

/// How long the screen must hold still before a question is keyed on a host
/// that sends no person stamp ([`HumanInput::Unknown`]): the stand-in for a
/// person's quiet, waited once per box.
const QUESTION_STANDIN_QUIET: Duration = Duration::from_secs(10);

/// What the loop remembers of the question dialog it is answering (module
/// docs, "A question dialog"): cleared when the worker works again, or a look
/// reads it at its composer with no dialog up ([`Session::look`]).
#[derive(Debug, Default)]
pub(super) struct QuestionMemory {
    /// The question the first-key wait (O4) was spent on ([`question_key`]).
    pub(super) keyed: Option<String>,
    /// The loop's last question key whose effect no read has shown yet.
    pub(super) pending: Option<QuestionPending>,
    /// The box the stand-in quiet was waited out for (a host that sends no
    /// person stamp).
    pub(super) standin: Option<String>,
    /// The box the journal's last `WAITING … rule=answer-recommended@v1`
    /// line was said for: one line per box, not one per wait.
    pub(super) waiting_said: Option<String>,
    /// The options the loop itself toggled, per multi-select question
    /// ([`question_key`]), for the person evidence R2 (d): a check it did
    /// not make, or one it made that reads unchecked, is a person's.
    pub(super) toggled: std::collections::BTreeMap<String, std::collections::BTreeSet<u8>>,
    /// The re-asked question ([`question_key`]) and its answer count whose
    /// pause ([`reask_pause`]) was waited: answered on the next decision.
    pub(super) reasked: Option<(String, usize)>,
    /// The answers the loop's own Enters gave this dialog (`<question> →
    /// <option>`), in order, the review's submit and multi-select toggles
    /// excepted: told once as `CHOSE` when the worker works again
    /// ([`Session::tell_answered`]), with a `story chose` to the window.
    pub(super) answered: Vec<String>,
    /// The dialog was a person's at some point (their input while it was up,
    /// a key yielded to them, or the dialog handed over): its end is theirs,
    /// never told as the policy's choice.
    pub(super) theirs: bool,
    /// The dialog as drawn ([`box_identity`]) when the policy last held it
    /// (`Session::question_held`: the session's word `ask`): a later decision
    /// that reads it drawn otherwise makes its end [`Self::theirs`]
    /// (`Session::decide_box`).
    pub(super) held_as: Option<String>,
    /// The policy texts the loop wrote, per question ([`question_key`]): the
    /// priority whose text a `send` of the loop's may have written into that
    /// question's free-text row, recorded when the server wrote it (`OK`),
    /// wrote part of it, or gave no answer, and never when it wrote nothing
    /// (`skipped`, `ERR busy …`, `ERR halted`). A text that reads as the
    /// policy's with no entry here is a person's (`OwnTextRemembered`).
    pub(super) typed: std::collections::BTreeMap<String, QuestionPriority>,
    /// The person count (the read's `human_seq`) each of those texts was
    /// fenced on (`send if-human=<n>`), per question: Codex's notes field
    /// drawing only the tail of the loop's text has it entered only while
    /// that count still stands ([`Session::press_codex_note`]).
    pub(super) typed_person: std::collections::BTreeMap<String, u64>,
    /// The latch: the answering word ([`Self::deciding`]) at the loop's first
    /// key or text on this dialog. A dialog whose effective word differs
    /// later is handed over; before the first key a new word simply applies.
    /// This loop's memory only: a restarted supervisor begins with none.
    pub(super) mode: Option<QuestionPriority>,
    /// The answering word of the decision in hand (`decide_box`): what the
    /// press latches ([`Self::mode`]) at its first write.
    pub(super) deciding: Option<QuestionPriority>,
    /// An answer the loop's own Enter gave was the worker's, decided by the
    /// policy text in its field: `CHOSE` names the latched word, not
    /// `recommended` ([`Session::tell_answered`]).
    pub(super) by_text: bool,
    /// The fallback latch, per question ([`question_key`]): questions whose
    /// tab the loop decided on its recommended option under a text word
    /// because the text would not fit the pane
    /// ([`super::super::policy::question::falls_back`]). Such a tab stays on
    /// that path until answered: its own toggle is never read back as a
    /// person's check, and its answer is never handed over half done.
    pub(super) fell_back: std::collections::BTreeSet<String>,
    /// The question read by its answers' roles whose note the loop's Enter
    /// submitted ([`Session::press_codex_note`]): a read that shows it still
    /// holding the note is the Enter not taken, never one to send again.
    pub(super) note_entered: Option<String>,
}

impl QuestionMemory {
    /// A person gave the session input while this dialog is up (R1): once a
    /// key, a text or an answering Enter of the loop's has reached it, the
    /// dialog's end is [`Self::theirs`]. Input before any write of the loop's
    /// changes nothing. Both places a person holds a question answer call
    /// this: the read the loop decides on ([`Session::question_hold`]) and the
    /// `status` read made for that decision ([`Session::held_for_person`], in
    /// [`Session::auto_read`]).
    pub(super) fn person_gave_input(&mut self) {
        if self.pending.is_some() || !self.answered.is_empty() || !self.typed.is_empty() {
            self.theirs = true;
        }
    }
}

/// What the loop remembers of the autocompact offer's stepper on the offer
/// now up ([`Session::stepper_check`]). The size list is the server's
/// (Claude Code 2.1.302 only asks that it hold the default and ascend), so
/// the target need not be on it, and the stepper clamps at both ends: a
/// key toward a size the list lacks oscillates around it, and one past an
/// end changes nothing. Neither is pressed again: a reversal, a key that did
/// not move the size, or more keys than the longest list takes, hands the
/// offer to a person.
#[derive(Debug, Default)]
pub(super) struct StepperTrack {
    /// The offer this is about: its title row and keep row, which hold
    /// while the stepper moves (the size, and so the box's identity, does
    /// not).
    offer: Option<String>,
    /// The key just decided and the size it was decided at, not yet seen
    /// pressed.
    deciding: Option<(&'static str, u32)>,
    /// The last stepper key pressed on this offer and the size it was
    /// pressed at.
    last: Option<(&'static str, u32)>,
    /// Stepper keys pressed on this offer.
    keys: usize,
}

/// The most stepper keys one offer takes: one fewer than the longest size
/// list Claude Code accepts (2.1.302: its built-in ladder's length plus two).
pub(super) const MAX_STEPPER_KEYS: usize = aterm_phase::autocompact::LADDER.len() + 1;

/// What a question write was ([`QuestionPending::kind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingKind {
    /// A focus move (`up`, `down`).
    Move,
    /// An Enter: an answer, or a multi-select toggle.
    Enter,
    /// The policy text (`send`): never written twice — its pending write
    /// is handed over, never retried ([`Session::decide_box`]).
    Text,
}

/// What the loop remembers of the decline it is carrying out
/// ([`Session::press_decline`]): cleared when the worker works again
/// ([`Session::look`]).
#[derive(Debug, Default)]
pub(super) struct DeclineMemory {
    /// The decline's last keystroke written whose effect no read has shown
    /// yet: never written a second time.
    pub(super) pending: Option<DeclinePending>,
    /// The re-raised box (`press.rs`'s `decline_box`) and its decline count
    /// whose pause ([`reask_pause`]) was waited: declined on the next
    /// decision.
    pub(super) again: Option<(String, usize)>,
}

/// A decline keystroke written, as the box stood when it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeclinePending {
    /// The box it was written on (`press.rs`, `decline_box`).
    pub(super) key: String,
    /// The keystroke, as decided on the read it was written on: a read that
    /// decides the very same one shows the box as it was keyed.
    pub(super) step: DeclineStep,
    /// The reads since that showed the box as it was keyed.
    pub(super) unseen: u32,
}

/// A question key written, as the box and its focus stood when it was
/// written (R3a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QuestionPending {
    /// [`box_identity`] of the read the key was decided on.
    pub(super) identity: String,
    /// The focus the key was written on.
    pub(super) focus: QuestionFocus,
    /// What was written.
    pub(super) kind: PendingKind,
    /// A read showed the box unchanged, and the screen then held still
    /// (`await idle 2000` latched) before the loop read again: the retry's
    /// premise.
    pub(super) waited: bool,
    /// Waits for the screen to hold still that ran out instead
    /// ([`MAX_UNSETTLED_WAITS`]).
    pub(super) unsettled: u32,
    /// How many times the key was written again on this box and focus
    /// ([`Session::question_pending`] counts it when the key goes).
    pub(super) retries: u32,
    /// The press back-off before the next try was waited
    /// ([`Session::box_back_off`]): the next read that still shows the box
    /// as keyed, the screen having held still, keys it again.
    pub(super) backed_off: bool,
}

/// Why this dialog's text priority is answered with its recommended option
/// instead, the whole dialog: the fences a typed answer needs that this
/// window did not name (`decide_box`, decision 12). `None`: it names them all.
fn text_unfenced(caps: &Caps, screen: &Screen) -> Option<String> {
    let mut missing: Vec<&str> = Vec::new();
    if caps.send_gen != Some(true) {
        missing.push("send if-gen=");
    }
    if screen.human_seq.is_none() {
        missing.push("the person count (human_seq)");
    }
    if screen.human == HumanInput::Unknown {
        missing.push("the person stamp (human_ms)");
    }
    (!missing.is_empty()).then(|| {
        format!(
            "this window cannot fence a typed answer: no {}",
            missing.join(", no ")
        )
    })
}

/// How a session's question is answered ([`Session::question_policy`]):
/// with which priority (`None`: it is the person's), whether the session's
/// own word said so, and, for a session word this build does not know, why
/// none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QuestionPolicy {
    /// The priority it is answered with; `None`: handed over.
    pub(super) answer: Option<QuestionPriority>,
    /// The session's `questions` word decided it (not the global policy).
    pub(super) by_session: bool,
    /// Why `answer` is `None` when the reason is not the plain `ask`.
    pub(super) why: Option<String>,
}

impl QuestionPolicy {
    /// The global policy alone, no session's word read: `answer_questions`,
    /// answered by `question_priority`.
    pub(super) fn global(opts: &SuperviseOpts) -> Self {
        Self {
            answer: opts
                .policy
                .answer_questions
                .then_some(opts.policy.question_priority),
            by_session: false,
            why: None,
        }
    }

    /// Why the question is the person's, in the words an escalation names
    /// (the session's own `ask`, or an unknown word), when the session's word
    /// handed it over; `None` otherwise.
    pub(super) fn session_reason(&self) -> Option<String> {
        (self.by_session && self.answer.is_none()).then(|| {
            self.why.clone().unwrap_or_else(|| {
                "this session's `questions` word is ask: a question is the person's to answer"
                    .to_string()
            })
        })
    }
}

/// A session's stored `questions` word as its policy: `ask` answers none; a
/// priority word ([`QuestionPriority::parse`]) answers with it; any other
/// word answers none and says so, never falling through to the global
/// policy.
fn session_word(word: &str) -> QuestionPolicy {
    if word == "ask" {
        return QuestionPolicy {
            answer: None,
            by_session: true,
            why: None,
        };
    }
    match QuestionPriority::parse(word) {
        Some(p) => QuestionPolicy {
            answer: Some(p),
            by_session: true,
            why: None,
        },
        None => QuestionPolicy {
            answer: None,
            by_session: true,
            why: Some(format!(
                "this session's `questions` word {word} is not one this supervisor knows: a \
                 question is the person's to answer"
            )),
        },
    }
}

/// What a question's answer waits on before it is keyed
/// ([`Session::question_hold`]).
enum Hold {
    /// Decide again after this (a person's quiet, the last key's effect, a
    /// re-ask's pause).
    Again,
    /// The question is the manager's, with the reason (`supervise`'s one
    /// look, and a person's begun answer).
    HandOver(String),
    /// Nobody is there to hand it to: decided again after the press
    /// back-off ([`Session::box_back_off`]), with the reason.
    BackOff(String),
}

/// One field of a bare `meta` reply, found by key: `None` when the reply has
/// no such field or it is unset (`-`), else the value, pct-decoded.
fn meta_word(stdout: &str, key: &str) -> Option<String> {
    let line = stdout.lines().find(|l| crate::is_ok_reply(l))?;
    let prefix = format!("{key}=");
    let raw = line
        .split_whitespace()
        .find_map(|w| w.strip_prefix(prefix.as_str()))?;
    (raw != "-" && !raw.is_empty()).then(|| super::super::report::pct_decode(raw))
}

/// Whether a bare `meta` reply names the field `key` at all, set or unset
/// (`key=-`): a host that keeps it.
fn meta_names(stdout: &str, key: &str) -> bool {
    let prefix = format!("{key}=");
    stdout
        .lines()
        .find(|l| crate::is_ok_reply(l))
        .is_some_and(|line| line.split_whitespace().any(|w| w.starts_with(&prefix)))
}

/// THE SESSION'S AUTOPILOT WORD as the loop last read it (`autopilot=` on a
/// bare `meta` reply, the word aterm-gui keeps: `meta set autopilot
/// off|cruise|land`), noted at EVERY bare `meta` the loop reads
/// ([`Session::note_meta`]: the start-up reconcile, a box's cwd, a
/// question's `questions` word) and read on purpose only where a decision
/// would act ([`Session::autopilot_read`]). What it decides: the turn-end
/// policy's mode ([`TurnEndReading::autopilot`]) and, at `off`, the loop's
/// park ([`Session::park_while_off`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct AutopilotWord {
    /// The last bare `meta` reply named `autopilot=` (set or `-`): the host
    /// keeps the word, so it is worth a read. A host that names none (an
    /// older build) can hold no word: nothing is asked of it, and the
    /// table's `[harness] autopilot` decides every session there.
    kept: bool,
    /// The session's own word, as the last reply that was read named it;
    /// `None` — unset, a word this build cannot read ([`Autopilot::parse`]),
    /// or no reply read yet — is the table's
    /// ([`SupervisorConfig::autopilot_for`]). A read the server refused
    /// changes nothing ([`Self::refused`]).
    word: Option<Autopilot>,
    /// A bare `meta` was read since the decision in hand began
    /// ([`Self::begin`]): its word is the decision's, and no second round
    /// trip is made for it.
    fresh: bool,
}

impl AutopilotWord {
    /// A decision begins: the word last read is no longer this decision's own.
    pub(super) fn begin(&mut self) {
        self.fresh = false;
    }

    /// A bare `meta` reply read: whether the host keeps the word, and the word.
    fn note(&mut self, meta: &str) {
        self.kept = meta_names(meta, "autopilot");
        self.word = meta_word(meta, "autopilot")
            .as_deref()
            .and_then(Autopilot::parse);
        self.fresh = true;
    }

    /// A bare `meta` the server refused: it says nothing of the word, so the
    /// word last read stands — a session the owner turned `off` is never
    /// handed back to the table's `cruise` by one refusal (failing open);
    /// with no word ever read, the table's decides. The decision in hand
    /// makes no second round trip for it.
    fn refused(&mut self) {
        self.fresh = true;
    }
}

/// A question's identity for the first-key wait and the re-ask cap: its
/// form and its question text; the review tab's, its rows as drawn (its
/// answers).
pub(super) fn question_key(dialog: &QuestionDialog, rows: &[String]) -> String {
    let form = match &dialog.form {
        QuestionForm::Single { .. } => "single",
        QuestionForm::Multi { .. } => "multi",
        QuestionForm::Preview { .. } => "preview",
        QuestionForm::Review { .. } => "review",
        QuestionForm::Unknown { .. } => "unknown",
    };
    match dialog.question() {
        Some(q) => format!("{form}\n{}", q.text),
        None => format!("{form}\n{}", box_identity(rows).unwrap_or_default()),
    }
}

/// Whether an Enter on `target` answers the question (it moves on or
/// submits) rather than toggling a multi-select option.
fn answers(dialog: &QuestionDialog, target: AnswerTarget) -> bool {
    !matches!(
        (&dialog.form, target),
        (QuestionForm::Multi { .. }, AnswerTarget::Option(_))
    )
}

/// The dialog's rows as drawn, the rule above its tab bar to its footer,
/// joined into one journal line (O3).
fn dialog_rows(rows: &[String]) -> Option<String> {
    let v2 = aterm_phase::parse_prompt_v2(rows)?;
    let (top, bottom) = v2.span;
    let top = top.saturating_sub(1);
    let bottom = bottom.min(rows.len().checked_sub(1)?);
    (top <= bottom).then(|| {
        rows[top..=bottom]
            .iter()
            .map(|r| r.trim_end())
            .collect::<Vec<_>>()
            .join(" ⏎ ")
    })
}

/// `DIALOG seq=<n> rule=<id> <rows>`: a question decision's dialog, for the
/// journal alone (O3).
fn note_dialog(review: &mut dyn Review, seq: u64, rule_id: &str, rows: &[String]) {
    if let Some(drawn) = dialog_rows(rows) {
        review.note(&format!("DIALOG seq={seq} rule={rule_id} {drawn}"));
    }
}

/// What "the same box" is after a press ([`Session::box_left`]): the box's
/// kind and its rows as [`box_rows`] keeps them, one per line — every row of
/// it as drawn, title to footer, trimmed, its body with it (an Edit's diff, a
/// tool's arguments, a Chrome box's action in its title, a workflow's script)
/// — less its auto-deny countdown, its focus marker and its blank rows
/// ([`box_rows`] says why each is left out). `None` with no box on `rows`.
pub(super) fn box_identity(rows: &[String]) -> Option<String> {
    let v2 = aterm_phase::parse_prompt_v2(rows)?;
    let mut key = format!("{}\n", v2.kind.name());
    for (_, row) in box_rows(&v2, rows) {
        key.push_str(row);
        key.push('\n');
    }
    Some(key)
}

/// The rows the box `v2` read on `rows` is made of, with their indexes: every
/// row of its span, title to footer (to its last row, for a footerless box),
/// trimmed, and a leading focus marker `❯` stripped — less what the vendor
/// redraws while the box waits, which is not the box:
///
/// * THE AUTO-DENY COUNTDOWN, which ticks every second: its `⚠` row and every
///   row it wrapped onto in a narrow grid (aterm-phase joins those into
///   [`aterm_phase::PromptV2::auto_deny`], which the ticking first row does
///   not equal, so each tick would read as a new box). A row is the
///   countdown's while the rows so far spell the start of it, led by its `⚠`
///   row.
/// * THE FOCUS MARKER `❯`: a focus move changes it, and an identity that
///   counted it would read the moved box as a new one.
/// * BLANK ROWS: within 5 ms of a taken `1` the vendor (2.1.283) wipes the
///   countdown and ONE of the two blank rows under it, and keeps the box
///   drawn some 550 ms more — an identity that counted blank rows read that
///   as the box leaving, and the box still on the screen was decided and
///   pressed a second time (measured live: every approved rm box).
///
/// The one reading of "the box's rows" the press's identity
/// ([`box_identity`]) and the loop's review key (`run.rs`'s `box_key`) share.
pub(super) fn box_rows<'a>(
    v2: &aterm_phase::PromptV2,
    rows: &'a [String],
) -> Vec<(usize, &'a str)> {
    let (top, bottom) = v2.span;
    let countdown = v2.auto_deny.as_deref();
    let mut out = Vec::new();
    // What is left of the countdown to spell, once its `⚠` row is seen.
    let mut rest: Option<&str> = None;
    for (i, r) in rows
        .iter()
        .enumerate()
        .take(bottom.saturating_add(1))
        .skip(top)
    {
        let t = r.trim();
        if t.is_empty() {
            continue;
        }
        let t = t.strip_prefix('❯').map_or(t, str::trim_start);
        let piece = t.trim_start_matches('⚠').trim();
        let left = match (rest, countdown) {
            (Some(left), _) => Some(left),
            (None, Some(c)) if t.starts_with('⚠') => Some(c),
            _ => None,
        };
        if let Some(left) = left.filter(|l| !piece.is_empty() && l.starts_with(piece)) {
            rest = Some(left[piece.len()..].trim_start()).filter(|l| !l.is_empty());
            continue;
        }
        rest = None;
        out.push((i, t));
    }
    out
}

/// What one `status` reply says for THE LAUNCH ([`Session::launch_call`]),
/// each field as the server wrote it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LaunchStatus {
    /// `launch_seq=` when `launch=person`: the person count the shell's last
    /// signed command start recorded, still the record's own (`since` once
    /// it moved). `None` for every other word, and a host that sends none.
    pub(super) person_seq: Option<u64>,
    /// `input=`: whether the program has read everything written to it.
    /// `None` when absent or `-` (a host that cannot probe).
    pub(super) input: Option<String>,
    /// `program=`: the foreground program, by the server's name for it.
    pub(super) program: Option<String>,
}

impl LaunchStatus {
    /// The [`LaunchStatus`] of one `status` reply.
    pub(super) fn of(stdout: &str) -> Self {
        let field = |f: &str| super::escalate::status_field(stdout, f);
        Self {
            person_seq: (field("launch") == Some("person"))
                .then(|| field("launch_seq").and_then(|n| n.parse().ok()))
                .flatten(),
            input: field("input").map(str::to_string),
            program: field("program").map(str::to_string),
        }
    }
}

/// What THE LAUNCH makes of a box a person's grace would hold.
pub(super) enum LaunchCall {
    /// No launch: the grace holds, as for any box.
    Held,
    /// A launch whose program has not yet read what was written to it
    /// (`input=pending|typeahead`): read and decided again shortly, the
    /// Autopilot word with it, never parked for the grace.
    Again,
    /// The launch: answered now, on this witness.
    Answer(crate::supervise::screen::LaunchWitness),
}

/// The label of the option the focus is on in the box on `rows`, if any.
pub(super) fn focused_label(program: Option<&str>, rows: &[String]) -> Option<String> {
    aterm_phase::read(program, rows, None)
        .prompt
        .and_then(|q| q.focused().map(|o| o.label.clone()))
}

/// The one line an approval names ([`Decision::Approve`]'s subject, its row
/// breaks as spaces), or `command` when the subject is empty.
fn shown_subject(subject: &str, command: &str) -> String {
    if subject.trim().is_empty() {
        command.to_string()
    } else {
        subject
            .split('\n')
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// What a question read by its answers' roles is told as once the loop's key
/// answered it (`CHOSE`, [`Session::tell_answered`]), and whether that key
/// was a note's Enter ([`Choice::Note`]: the latched word's text). It is the
/// decision's subject with the title it opens with (`Question 1/1 (1
/// unanswered)`, `Request user input Color`) given as the question asked,
/// where the reader read one: `<question> → <answer>`, as a dialog's answer is
/// told. `None` for every other key: a dialog's answer ([`Choice::Answer`]) is
/// told by the dialog's own bookkeeping, a box's key or a decline answers no
/// question, and the Enter on Muse Code's review sends answers told already.
fn told_by_role(act: &Act, p: &aterm_phase::PromptV2, what: &str) -> Option<(String, bool)> {
    // Muse Code's review sends answers already told (the note's Enter).
    if aterm_phase::muse::is_review(p) {
        return None;
    }
    let note = match act {
        Act::Approve { choice, .. } => match choice {
            Choice::Digit(_) | Choice::Focus { .. } => false,
            Choice::Note { .. } => true,
            Choice::Answer(_) | Choice::FocusKey { .. } => return None,
        },
        Act::Decline { .. } => return None,
    };
    let asked = one_line(&p.description);
    let told = match what
        .strip_prefix(p.title.as_str())
        .and_then(|rest| rest.strip_prefix(" → "))
    {
        Some(answer) if !asked.trim().is_empty() => format!("{} → {answer}", asked.trim()),
        _ => what.to_string(),
    };
    Some((told, note))
}

/// What [`Session::box_facts`] reads of the worker for the box at hand.
#[derive(Debug, Clone, Copy)]
struct Reads {
    /// A Bash box whose command may run git ([`may_run_git`]).
    git: bool,
    /// A Bash box.
    bash: bool,
    /// A Claude Code box drawn with no ask reason ([`unvoiced_tools`]).
    asks: bool,
}

/// The box needs the session's working directory: the vendor's rm/rmdir
/// circuit breaker of any kind ([`aterm_phase::PromptV2::rm_breaker`]), the
/// folder-trust dialog, and a Bash box that may run git ([`may_run_git`]) —
/// and, where the worker is read from the session, every Bash box, whose
/// shell startup is read with the project's Claude Code settings
/// ([`ApprovalCtx::shell`]; the caller adds that).
fn needs_cwd(p: &aterm_phase::PromptV2) -> bool {
    p.kind == PromptKind::Trust || p.rm_breaker().is_some() || may_run_git(p)
}

/// A Bash box whose command may run git: its configuration is read where the
/// session's Bash tool stands as well ([`ApprovalCtx::shell_cwds`]).
fn may_run_git(p: &aterm_phase::PromptV2) -> bool {
    p.kind == PromptKind::Bash && p.command_rows.iter().any(|r| r.contains("git"))
}

/// The session's `s-…` id and the foreground process group of its PTY, off a
/// `who` reply's rows ([`roster_rows`]): the row of `stable` (the `s-…` id
/// the loop addresses, never reused) where the loop has one, else the row
/// whose instance-local id is `local`. Refused when the roster cannot be
/// read, when the kernel could not read the group, and when another row
/// names the same group ([`unique_tab_for_group`]).
pub(super) fn session_group(
    rows: &str,
    stable: Option<&str>,
    local: Option<&str>,
) -> Result<(String, u32), String> {
    let rows = roster_rows(rows).ok_or("`who` answered no roster this check can read")?;
    let sid = match (stable, local) {
        (Some(stable), _) => stable.to_string(),
        (None, Some(local)) => rows
            .iter()
            .find(|(id, _)| id.to_string() == local)
            .map(|(_, tab)| tab.sid.clone())
            .ok_or_else(|| format!("session {local} is not on the instance's roster (`who`)"))?,
        (None, None) => return Err("the session's id is unknown (`status` named none)".to_string()),
    };
    let tabs: Vec<LiveTab> = rows.into_iter().map(|(_, tab)| tab).collect();
    let group = tabs
        .iter()
        .find(|tab| tab.sid == sid)
        .ok_or_else(|| format!("session {sid} is not on the instance's roster (`who`)"))?
        .fgpgid
        .ok_or_else(|| format!("{sid}'s foreground process group is unknown"))?;
    if unique_tab_for_group(&tabs, group) != Some(sid.as_str()) {
        return Err(format!(
            "{sid}'s foreground process group {group} is another session's too"
        ));
    }
    let group = u32::try_from(group)
        .map_err(|_| format!("{sid}'s foreground process group {group} is out of range"))?;
    Ok((sid, group))
}

/// How long a box on a screen whose named program is no agent waits for
/// the server's own verdict (`await agent prompt`) before it is judged as
/// that program's.
const PROGRAM_SETTLE: Duration = Duration::from_millis(2000);

/// The pid of the codex-alab a `status` line names as its PROVEN self-claim
/// (`supervisor_proven=1` beside `supervisor=codex-alab@<pid>`, the holder
/// percent-encoded as every `status` field is); `None` for any other claim,
/// an unproven one, or none.
pub(super) fn proven_alab(status: &str) -> Option<u32> {
    if super::escalate::status_field(status, "supervisor_proven") != Some("1") {
        return None;
    }
    let holder = super::escalate::status_field(status, "supervisor")?;
    holder
        .strip_prefix("codex-alab%40")
        .or_else(|| holder.strip_prefix("codex-alab@"))?
        .parse()
        .ok()
}

impl<C: Ctl> Session<'_, C> {
    /// The session's foreground program as `status program=` names it, or
    /// `None`: from a host that does not publish it (the screen then names
    /// the reader, [`aterm_phase::identify`]), and when the server's own
    /// verdict (`agent=`) already identified an agent on this screen while
    /// `program=` names something else (an agent `exec`ed in place keeps its
    /// launcher's name until the screen moves). A shell showing a captured
    /// box gets no verdict (`agent=-`), so it is never judged as an agent.
    ///
    /// The same read says when a person last typed into the session
    /// (`human_ms=`, [`Session::person`]), so the person is never older than
    /// the decision.
    pub(super) fn foreground_program(&mut self) -> Result<Option<String>, Fail> {
        let r = self.call(&["status"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("status failed: {}", r.stderr.trim())));
        }
        if !r.ok() {
            self.person = None;
            self.launch_status = LaunchStatus::default();
            self.status_sid = None;
            self.halted = false;
            self.proven_alab = None;
            return Ok(None);
        }
        let field = |f: &str| super::escalate::status_field(&r.stdout, f).map(str::to_string);
        self.proven_alab = proven_alab(&r.stdout);
        self.launch_status = LaunchStatus::of(&r.stdout);
        self.halted = field("hold").as_deref() == Some("1");
        self.status_sid = field("sid").filter(|s| s != "-");
        self.person = field("human_ms")
            .and_then(|ms| ms.parse::<u64>().ok())
            .and_then(|ms| self.fx.mono().checked_sub(Duration::from_millis(ms)));
        // Another driver's hand on the session (a drive lease or a turn a
        // named driver typed: `hand=lease:<holder>`, `hand=turn:<id>:<holder>`)
        // holds the loop as a person's keystroke does, so the loop never
        // fights a manager driving the worker. An owner-class turn names
        // nobody: this loop's own continuations are typed so.
        // (A halt is the server's to refuse: `ERR halted` parks the loop.)
        self.driver = crate::harness::relaunch::driver_hand(&r.stdout);
        if self.driver {
            self.person = Some(self.fx.mono());
        }
        let program = field("program");
        let named_agent = program
            .as_deref()
            .and_then(aterm_phase::program_of)
            .is_some();
        Ok(if field("agent").is_some() && !named_agent {
            None
        } else {
            program
        })
    }

    /// What the loop learns of the box at hand before the policy decides it
    /// ([`BoxFacts`], the effectful half of the approval policy's context —
    /// [`ApprovalCtx::from`] is the pure half): the session's cwd (`meta`
    /// read fresh when `fresh_cwd`); for a box that may run git (`reads.git`)
    /// the worker ([`ApprovalCtx::worker`]) and the directories its Bash tool
    /// may stand in ([`ApprovalCtx::shell_cwds`]); for any Bash box
    /// (`reads.bash`) the worker and, when read from the session
    /// ([`WorkerSource::Session`]), its Bash tool's shell startup
    /// ([`ApprovalCtx::shell`]); for a box Claude Code draws no ask reason on
    /// (`reads.asks`) the ask rules and PreToolUse hooks of the worker's own
    /// settings ([`ApprovalCtx::owner_asks`]); the footer's mode, the program
    /// the box was read with, and whether a limit reset was passed over here.
    /// The rest of the facts — the pane, the question's answer, a transcript
    /// candidate — the decision fills in as it reads them
    /// ([`Self::decide_box`]).
    fn box_facts(&mut self, fresh_cwd: bool, reads: Reads) -> Result<BoxFacts, Fail> {
        let Reads { git, bash, asks } = reads;
        if fresh_cwd {
            let r = self.call(&["meta"])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!("meta failed: {}", r.stderr.trim())));
            }
            if r.ok() {
                self.note_meta(&r.stdout);
            } else {
                self.cwd = None;
                self.autopilot.refused();
            }
        }
        let cwd = self.cwd.clone();
        let mut facts = BoxFacts {
            program: self.program.clone(),
            cwd: cwd.clone(),
            bypass: self.footer == Some(crate::harness::lights::Mode::Bypass),
            limit_reset_passed: self.reset_passed_here(),
            ..BoxFacts::default()
        };
        if git || bash || asks {
            // Every look the policy makes at the machine — here and inside
            // `decide` — goes through the loop's probe: the live look, or one
            // that tapes each derived answer when the run records.
            let probe = self.fx.policy_probe();
            let source = self.setup.approval_env.worker.clone();
            let session = matches!(source, WorkerSource::Session);
            let worker = match source {
                WorkerSource::Session => self.worker_env(),
                WorkerSource::Fixed(worker) => worker,
            };
            if git && let (Ok(w), Some(cwd)) = (&worker, cwd.as_deref()) {
                facts.shell_cwds = probe.shell_cwds(w, cwd);
            }
            if bash && session {
                facts.shell = Some(match (&worker, cwd.as_deref()) {
                    (Ok(w), Some(cwd)) => probe.shell_startup(w, cwd),
                    (Err(why), _) => Err(format!("the worker's environment is unknown: {why}")),
                    (Ok(_), None) => Err("the session's cwd is unknown (meta cwd=-)".to_string()),
                });
            }
            if asks {
                facts.owner_asks = Some(match (&worker, cwd.as_deref()) {
                    (Ok(w), Some(cwd)) => probe.owner_asks(w, cwd),
                    (Err(why), _) => Err(format!("its environment is unknown: {why}")),
                    (Ok(_), None) => Err("its directory is unknown (meta cwd=-)".to_string()),
                });
            }
            facts.worker = Some(worker);
        }
        Ok(facts)
    }

    /// The approval policy's context for the box `facts` describe: built
    /// from them alone ([`ApprovalCtx::from`]), with the loop's own probe.
    fn approval_ctx(
        &self,
        facts: &BoxFacts,
        opts: &SuperviseOpts,
        allow: &[String],
    ) -> ApprovalCtx {
        ApprovalCtx::from(
            &opts.policy,
            &self.setup.approval_env,
            allow,
            facts,
            self.fx.policy_probe(),
        )
    }

    /// The worker's environment ([`WorkerEnv`]): the environment at exec of
    /// the session's foreground process group's leader (`who`'s `fgpgid=` on
    /// the session's row, [`session_group`]), read through the kernel and
    /// bound to this session by the `ATERM_PARENT_SESSION_ID` every aterm
    /// session hands its shell — the policy probe's
    /// [`super::super::policy::PolicyProbe::worker_proc`], so a harness tape
    /// holds the worker as a digest and a pid and never its variables. A
    /// process whose variable names another session, or none, is not taken
    /// for the worker. `Err` says what could not be read or did not hold.
    pub(crate) fn worker_env(&mut self) -> Result<WorkerEnv, String> {
        let stable = self
            .setup
            .sid
            .as_deref()
            .and_then(|s| s.strip_prefix('@'))
            .filter(|s| s.starts_with("s-"))
            .map(str::to_string);
        let r = self
            .ctl
            .call(&["who"])
            .map_err(|e| format!("`who` failed: {e}"))?;
        if !r.ok() {
            return Err(format!("`who` refused: {}", r.stderr.trim()));
        }
        let (sid, group) = session_group(&r.stdout, stable.as_deref(), self.status_sid.as_deref())?;
        self.fx.policy_probe().worker_proc(group, &sid)
    }

    /// A tool candidate inferred from the host's transcript ([`box_call::drew`]),
    /// without current-mount identity. A compatible screen is checked first,
    /// then candidate reads may repeat while [`box_call::lagging`] holds.
    /// Remembering and spending a candidate across screen changes only avoids
    /// reusing it; neither that bookkeeping nor a complete EOF proves which
    /// call owns the current box. The policy keeps every possible tool's
    /// settings and hook hold, so [`proof_would_lift`] does not admit this
    /// read path for those holds.
    fn box_call(
        &mut self,
        p: &PromptV2,
        opts: &SuperviseOpts,
        home: Option<&Path>,
    ) -> Option<String> {
        if !box_call::box_may_prove(p) {
            return None;
        }
        let host = opts.idle_host.as_deref()?;
        let mut pending = box_call::pending_calls(&host.transcript_tail()?);
        for _ in 0..box_call::LAG_TRIES {
            if !box_call::lagging(p, &pending, &self.calls_spent) {
                break;
            }
            self.fx.sleep(self.setup.box_call_step);
            pending = box_call::pending_calls(&host.transcript_tail()?);
        }
        let call = box_call::drew(p, &pending, &self.calls_spent, home)?;
        self.box_proved = Some(call.id);
        Some(call.tool)
    }

    /// The box on the screen has left: the next one mounts afresh
    /// ([`Self::box_young`]), and any remembered transcript candidate is spent
    /// ([`Self::box_call`]).
    pub(super) fn box_gone(&mut self) {
        self.box_first_seen = None;
        self.spend_proved();
    }

    /// Spend the remembered transcript candidate, without asserting its mount.
    fn spend_proved(&mut self) {
        if let Some(id) = self.box_proved.take() {
            self.calls_spent.push(id);
            let over = self.calls_spent.len().saturating_sub(SPENT_CALLS);
            self.calls_spent.drain(..over);
        }
    }

    /// The decision on the box on `screen`: read by the reader for the
    /// session's foreground program, then [`decide`]d. Under `approve =
    /// "none"` nothing is read and the box escalates — but a question, or
    /// the usage-limit dialog while `limit_wait` is on (or `limit_reset`,
    /// at a mount its reset was not passed over for), each its own rule's,
    /// and Claude Code's MCP server dialog, which the policy escalates
    /// itself, its reason saying what every answer of it keeps.
    ///
    /// Where the decision is the usage-limit dialog's GRANT ROW
    /// ([`RULE_LIMIT_RESET`]), the decision the same read gets WITHOUT that
    /// rule is kept beside it ([`Session::reset_fallback`]): what the dialog
    /// is answered with if the reset is passed over before its first key
    /// ([`Session::reset_take`]) — the answer of a build with the key off,
    /// with no second read and no second `status`.
    pub(super) fn decide_box(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        allow: &[String],
    ) -> Result<Decision, Fail> {
        // Every decision starts with the dialog held by no policy and no word
        // deciding it, before the early escalations below, which read
        // `question_held` ([`Self::auto_read`]'s Escalate arm).
        self.question_held = false;
        self.question.deciding = None;
        self.reset_fallback = None;
        self.decided_on = None;
        // A question is no permission, and neither is the usage-limit
        // dialog: no level limits either (only `answer_questions`,
        // `limit_wait` and `limit_reset`), so under `none` the screen is read
        // (no request) for one before anything is asked. Claude Code's MCP
        // server dialog is read so too: the policy escalates it at every
        // level, and its reason (every answer, Esc too, saved for good) is
        // what the owner's badge must carry, never `none`'s own.
        // The reset's rule is the GRANT ROW's: a dialog that draws none is
        // read for it by nobody.
        let limit_reset = opts.policy.limit_reset && !self.reset_passed_here();
        let own_rule = |limit_reset: bool| {
            aterm_phase::read(None, &screen.rows, None)
                .prompt
                .is_some_and(|p| {
                    p.kind == PromptKind::Question
                        || p.kind == PromptKind::McpServer
                        || p.kind == PromptKind::AutocompactOffer
                            && opts.policy.autocompact_offer.is_some()
                        || p.kind == PromptKind::UsageLimit
                            && (opts.policy.limit_wait
                                || limit_reset && aterm_phase::reset_row(&p).is_some())
                })
        };
        let owners = || Decision::Escalate {
            reason: "approve = \"none\": every box is the owner's".to_string(),
        };
        if opts.policy.approve == Approve::None && !own_rule(limit_reset) {
            return Ok(owners());
        }
        if let Some(holder) = self.claim.watching_behind() {
            return Ok(Decision::Escalate {
                reason: format!("another supervisor ({holder}) answers this session"),
            });
        }
        self.program = self.foreground_program()?;
        let mut reading = aterm_phase::read(
            self.program.as_deref(),
            &screen.rows,
            Some(screen.cursor_col),
        );
        if reading.prompt.is_none() && aterm_phase::read(None, &screen.rows, None).prompt.is_some()
        {
            // The screen shows an agent's box and the named program is none: a
            // shell showing a capture, or an agent just started whose name the
            // server has not resolved yet. Wait for the server's verdict
            // (`await agent prompt`, bounded), then read the name once more.
            let settle = PROGRAM_SETTLE.as_millis().to_string();
            let r = self.call(&["await", "agent", "prompt", "timeout", &settle])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!(
                    "await agent failed: {}",
                    r.stderr.trim()
                )));
            }
            if r.ok() {
                self.program = self.foreground_program()?;
                reading = aterm_phase::read(
                    self.program.as_deref(),
                    &screen.rows,
                    Some(screen.cursor_col),
                );
            }
        }
        let git = reading.prompt.as_ref().is_some_and(may_run_git);
        let bash = reading
            .prompt
            .as_ref()
            .is_some_and(|p| p.kind == PromptKind::Bash);
        // A box Claude Code draws no ask reason on: the agent's own settings
        // say whether an ask rule sent it (`policy::approval::settings_ask`).
        let asks = match reading.program {
            Program::Claude => reading
                .prompt
                .as_ref()
                .is_some_and(|p| !unvoiced_tools(p).is_empty()),
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        let fresh_cwd = reading.prompt.as_ref().is_some_and(needs_cwd)
            || asks
            || bash && matches!(self.setup.approval_env.worker, WorkerSource::Session);
        let mut facts = self.box_facts(fresh_cwd, Reads { git, bash, asks })?;
        facts.screen_cols = screen
            .cols
            .unwrap_or_else(|| super::super::policy::question::widest_row(&screen.rows));
        // The pane's height: a `tail=` read's rows are its last 40.
        facts.screen_rows = screen.pane_rows.unwrap_or(screen.first + screen.rows.len());
        // The session's word: a question dialog is answered by this session's
        // own `questions` word where it has one (`ask` hands it to a person,
        // a priority word answers by that priority), over the table's
        // `answer_questions` and `question_priority`, but never over a loop's
        // own `--no-answer` or a value the reader could not take
        // (`session_questions`). A dialog the policy hands over is held: the
        // wait for it re-reads the word ([`Session::wait_for_next`]).
        let mut said_by_session = None;
        let mut fallback: Option<(QuestionPriority, String)> = None;
        // The question a text word decides, for the fallback latch.
        let mut latchable: Option<(String, QuestionDialog)> = None;
        if let Some(prompt) = reading
            .prompt
            .as_ref()
            .filter(|p| p.kind == PromptKind::Question)
        {
            let policy = if opts.policy.session_questions {
                let policy = self.question_policy(opts)?;
                self.question_held = policy.answer.is_none();
                // The dialog the policy held has changed since (a tab
                // answered, a check toggled, while the loop's hands were off
                // it): someone else answered there, and its end is theirs.
                let drawn = box_identity(&screen.rows);
                if self
                    .question
                    .held_as
                    .as_ref()
                    .is_some_and(|was| Some(was) != drawn.as_ref())
                {
                    self.question.theirs = true;
                }
                self.question.held_as = if self.question_held { drawn } else { None };
                policy
            } else {
                QuestionPolicy::global(opts)
            };
            let mut question = QuestionFacts {
                answer: policy.answer.is_some(),
                priority: QuestionPriority::default(),
                own: None,
                fell_back: false,
            };
            said_by_session = policy.session_reason();
            if let Some(word) = policy.answer {
                // How it is answered: the word, and what the loop remembers
                // writing into this question's free-text row.
                question.priority = word;
                self.question.deciding = Some(word);
                let this = prompt
                    .question_dialog
                    .as_ref()
                    .map(|d| (question_key(d, &screen.rows), d.focus()));
                // A question read by its answers' roles (Codex's, Muse
                // Code's) is remembered by its words: the note the loop wrote
                // into it, and its fallback latch (`press_codex_note`). Muse
                // Code's review is remembered by the question whose note the
                // loop entered, the review titled as that question is.
                let key = this.as_ref().map(|(k, _)| k.clone()).or_else(|| {
                    prompt.question_dialog.is_none().then(|| {
                        let entered = self.question.note_entered.clone().filter(|k| {
                            aterm_phase::muse::is_review(prompt)
                                && k.lines().next() == Some(prompt.title.as_str())
                        });
                        entered.unwrap_or_else(|| {
                            super::super::policy::question::roles_question_key(prompt)
                        })
                    })
                });
                question.own = key
                    .as_ref()
                    .and_then(|k| self.question.typed.get(k).copied());
                question.fell_back = key
                    .as_ref()
                    .is_some_and(|k| self.question.fell_back.contains(k));
                // The latch: the word at the loop's first key on this dialog
                // is the dialog's; a word changed since is a person's call
                // (decision 13).
                if let Some(latched) = self.question.mode.filter(|m| *m != word) {
                    return Ok(Decision::Escalate {
                        reason: format!(
                            "the question policy was {} at the supervisor's first key on this \
                             dialog and is {} now: a person answers it",
                            latched.word(),
                            word.word()
                        ),
                    });
                }
                // R3a for the text: the loop's policy text written, and the
                // box still as it was keyed, so the text was not taken. It is
                // never written twice: handed over. The other two guards are
                // the decider's ("gone": a field the loop wrote that reads
                // empty, `policy/question.rs`'s `choose_text`) and the press's
                // (`press.rs`: a question the loop wrote a text into is never
                // written into again).
                if let (Some((_, focus)), Some(pending)) = (&this, &self.question.pending)
                    && pending.kind == PendingKind::Text
                    && pending.focus == *focus
                    && box_identity(&screen.rows).as_deref() == Some(pending.identity.as_str())
                {
                    return Ok(Decision::Escalate {
                        reason: "the dialog did not take the policy text: a text is never written \
                                 twice"
                            .to_string(),
                    });
                }
                // A text priority needs every fence a typed answer rests on
                // (the send's generation fence, the person fence on the send
                // and the Enter, and the person stamp): a window that lacks
                // one answers the whole dialog with its recommended option
                // instead, told (decision 12).
                if word.text().is_some() {
                    let _ = self.gen_fence_known()?;
                    let _ = self.send_fence_known()?;
                    if let Some(why) = text_unfenced(&self.caps, screen) {
                        question.priority = QuestionPriority::Recommended;
                        fallback = Some((word, why));
                    } else if let (Some((key, _)), Some(dialog)) =
                        (this, prompt.question_dialog.clone())
                    {
                        latchable = Some((key, dialog));
                    }
                }
            }
            facts.question = Some(question);
        }
        let mut ctx = self.approval_ctx(&facts, opts, allow);
        // Query the actual decider before reading a transcript candidate.
        // Candidates carry no current-mount identity and cannot lift any
        // possible-tool settings or hook hold, so that policy admits no
        // transcript read here ([`proof_would_lift`]).
        if asks
            && let Some(p) = reading.prompt.as_ref()
            && proof_would_lift(&reading, &screen.rows, &ctx)
        {
            facts.box_call = self.box_call(p, opts, ctx.home.as_deref());
            ctx = self.approval_ctx(&facts, opts, allow);
        }
        let decision = decide(&reading, &screen.rows, &ctx);
        // What it was decided on, for the decision's mark on a harness tape
        // ([`Self::auto_read`]): the context is these facts' alone, so a
        // diagnostic reader can decide again from the recorded facts.
        self.decided_on = Some(facts);
        // The grant row's rule: the decision this read gets without it, kept
        // for a reset passed over before its first key. Under `none` a
        // dialog read for its grant row alone, and not decided by the row's
        // rule (its head off the screen, no option focused), is the owner's
        // by the words of a loop with the key off.
        let owners_box = opts.policy.approve == Approve::None && !own_rule(false);
        if matches!(&decision, Decision::Approve { rule_id, .. } if *rule_id == RULE_LIMIT_RESET) {
            self.reset_fallback = Some(if owners_box {
                owners()
            } else {
                ctx.limit_reset = false;
                decide(&reading, &screen.rows, &ctx)
            });
        } else if owners_box {
            return Ok(owners());
        }
        // The fallback latch: a tab this text word decided on its recommended
        // option (the text would not fit) stays there.
        if let (
            Some((key, dialog)),
            Decision::Approve {
                choice: Choice::Answer(answer),
                ..
            },
        ) = (latchable, &decision)
            && super::super::policy::question::falls_back(&dialog, answer)
        {
            self.question.fell_back.insert(key);
        }
        let decision = if screen.human_seq.is_none()
            && matches!(
                &decision,
                Decision::Approve { rule_id, .. } if *rule_id == RULE_AUTOCOMPACT_OFFER
            ) {
            Decision::Escalate {
                reason: "the autocompact offer's read carries no person count to fence its keys"
                    .to_string(),
            }
        } else {
            decision
        };
        let decision = self.stepper_check(
            &reading,
            &screen.rows,
            opts.policy.autocompact_offer,
            decision,
        );
        let decision = if is_muse(&reading.program)
            && screen.human_seq.is_none()
            && matches!(
                &decision,
                Decision::Approve {
                    choice: Choice::Focus { .. },
                    ..
                }
            ) {
            Decision::Escalate {
                reason: "the Muse read carries no person count to fence its focus keys".to_string(),
            }
        } else {
            decision
        };
        Ok(match decision {
            // The escalation names the word that decided it: the session's
            // own `ask` (or a word this build does not know), not a switch
            // the owner left on.
            Decision::Escalate { .. } if said_by_session.is_some() => Decision::Escalate {
                reason: said_by_session.unwrap_or_default(),
            },
            // The recommended option answered under a text word this window
            // cannot type for: ledgered under the word's own rule, the
            // fallback said as the decider says its own
            // (`<question> → <label> (recommended: <why>)`).
            decision @ Decision::Approve { rule_id, .. }
                if is_question_rule(rule_id) && fallback.is_some() =>
            {
                let (word, why) = fallback.unwrap_or_default();
                fall_back_to(decision, word, &why)
            }
            decision => decision,
        })
    }

    /// A question dialog the loop's own keys answered, done (the worker works
    /// again) with no person taking part: said once as `CHOSE seq=<n>
    /// rule=<rule> policy=<word> <question → answer | …>`, printed and
    /// journaled, and told to the window as `story chose <word>` (its chime
    /// and rim pulse), through a `watch`'s teller or, in the hosted loop, one
    /// `story` request. The rule is the latched word's
    /// ([`QuestionMemory::mode`]); the word is the latched text word when a
    /// tab was answered from the policy text, else `recommended`.
    pub(super) fn tell_answered(
        &mut self,
        done: &QuestionMemory,
        seq: u64,
        notes: Option<&Path>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let said = done.answered.join(" | ");
        let latched = done.mode.unwrap_or_default();
        let rule = latched.rule_id();
        let word = if done.by_text {
            latched.word()
        } else {
            QuestionPriority::Recommended.word()
        };
        let line = format!(
            "CHOSE seq={seq} rule={rule} policy={word} {}",
            clip(&one_line(&said))
        );
        review.say(&line).map_err(Fail::Hard)?;
        if self.tell_own
            && let Some((verb, text)) = story_of(&line)
        {
            // Best-effort, as the teller's post is: a host without the word
            // answers `ERR usage`, and nothing printed changes.
            let _ = self.call(&["story", verb, &text]);
        }
        self.append_note(notes, &format!("chose ({rule}, {word}): {said}"))?;
        Ok(())
    }

    /// The question dialog in hand is done (the worker works again, read
    /// busy, or sits at its composer with no dialog up): what the loop
    /// remembered of it is forgotten, and a dialog the loop's own keys
    /// answered is told once ([`Self::tell_answered`], at `seq`), never one a
    /// person took part in (`theirs`) or one the policy still held when it
    /// ended (`question_held`). Called at the turn's first busy read
    /// ([`Self::await_turn_from`]) and, as the fallback, at the look's worked
    /// edge, and at a look that reads the worker at its composer with no
    /// dialog up ([`Self::look`]: a dialog gone with no answer of the loop's,
    /// its text forgotten with it).
    pub(super) fn question_done(
        &mut self,
        seq: u64,
        notes: Option<&Path>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let done = std::mem::take(&mut self.question);
        let held_to_end = std::mem::take(&mut self.question_held);
        if !done.answered.is_empty() && !done.theirs && !held_to_end {
            self.tell_answered(&done, seq, notes, review)?;
        }
        Ok(())
    }

    /// Whether this session's question dialog is answered
    /// ([`Self::question_policy`]'s answer is some priority): the hand-back
    /// poll's question.
    pub(super) fn question_policy_answers(&mut self, opts: &SuperviseOpts) -> Result<bool, Fail> {
        Ok(self.question_policy(opts)?.answer.is_some())
    }

    /// The one resolver of how this session's questions are answered (its
    /// dialogs, and those it asks in prose by its priority words): its own
    /// `questions` word, read by key off a bare `meta` reply (its `cwd=`
    /// noted on the way), where `ask` answers none, `recommended`,
    /// `long-term` or `short-term` answers with that priority, and any other
    /// word answers none and says so; else the table's `answer_questions`,
    /// answered by `question_priority`. A meta with no such field, an unset
    /// one (`-`) and a refused read all fall to the table's.
    pub(super) fn question_policy(&mut self, opts: &SuperviseOpts) -> Result<QuestionPolicy, Fail> {
        let global = QuestionPolicy::global(opts);
        let r = self.call(&["meta"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("meta failed: {}", r.stderr.trim())));
        }
        if !r.ok() {
            self.autopilot.refused();
            return Ok(global);
        }
        self.note_meta(&r.stdout);
        Ok(match meta_word(&r.stdout, "questions") {
            None => global,
            Some(word) => session_word(&word),
        })
    }

    /// What a bare `meta` reply says of the session that the loop keeps: its
    /// working directory ([`Self::note_cwd`]) and its autopilot word
    /// ([`AutopilotWord`]).
    pub(super) fn note_meta(&mut self, meta: &str) {
        self.note_cwd(meta);
        self.autopilot.note(meta);
    }

    /// The session's Autopilot as the loop last read it: its own word, else
    /// the table's `[harness] autopilot` ([`SupervisorConfig::autopilot_for`]).
    pub(super) fn autopilot(&self, opts: &SuperviseOpts) -> Autopilot {
        opts.policy.autopilot_for(self.autopilot.word)
    }

    /// [`Self::autopilot`] for a decision about to act: read fresh — one bare
    /// `meta` — unless a bare `meta` was read since the decision began
    /// ([`AutopilotWord::begin`]; the box's cwd, the question's word: that
    /// reply's word is the decision's) or the host keeps no word (nothing to
    /// read: the table's decides). A refused read keeps the word last read
    /// ([`AutopilotWord::refused`]: the table's only where none ever was);
    /// one not served is the outage it is.
    pub(super) fn autopilot_read(&mut self, opts: &SuperviseOpts) -> Result<Autopilot, Fail> {
        if self.autopilot.kept && !self.autopilot.fresh {
            self.read_autopilot_word()?;
        }
        Ok(self.autopilot(opts))
    }

    /// A busy-read action is a new decision too: a CLI watcher can spend
    /// hours in one turn while the owner changes the session's word. Read it
    /// at that boundary, before a survey key or a host step.
    /// A refused read leaves the remembered word intact for parking, but
    /// proves no admission for this action. Older hosts that keep no word
    /// still use the table without an unsupported round trip.
    pub(super) fn autopilot_allows_busy(&mut self, opts: &SuperviseOpts) -> Result<bool, Fail> {
        self.autopilot.begin();
        let known = !self.autopilot.kept || self.read_autopilot_word()?;
        Ok(known && self.autopilot(opts) != Autopilot::Off)
    }

    /// One bare metadata read: whether the host actually answered it.
    fn read_autopilot_word(&mut self) -> Result<bool, Fail> {
        let r = self.call(&["meta"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("meta failed: {}", r.stderr.trim())));
        }
        if r.ok() {
            self.note_meta(&r.stdout);
        } else {
            self.autopilot.refused();
        }
        Ok(r.ok())
    }

    /// One row of the approval ledger — the loop's ONE writer of it — and,
    /// where the row is an act of the harness's
    /// ([`crate::harness::acts::sort`]: a box approved or declined, a
    /// question answered, the work kept moving or resumed, Land's
    /// instruction or its nudge), that act told to the loop's host
    /// ([`IdleHost::act_taken`], design §9.2.3 item 2) once the row is
    /// written, so the window can light its Autopilot chip for a moment. A
    /// question dialog's keys are three rows and three tells (the window
    /// coalesces them into one label); `skipped`, `deferred`, `waiting` and
    /// `escalated` rows tell nothing. A loop with no host — a CLI `watch` or
    /// `supervise` — sorts nothing and tells nothing.
    pub(super) fn ledger_row(
        &mut self,
        rule_id: &str,
        outcome: approvals::Outcome,
        command: &str,
        reason: &str,
        box_seq: u64,
    ) {
        // An act typed closes every exchange left to a person, as its row
        // closes them in the ledger (`approvals::open_left`): the accounts
        // probe's `/usage` asks the model nothing.
        if outcome == approvals::Outcome::Typed
            && rule_id != crate::supervise::policy::turn_end::RULE_USAGE_PROBE
        {
            self.left_keys.clear();
        }
        let row = self.ledger.write(
            self.fx.stamp_ms(),
            &approvals::Row {
                rule_id,
                outcome,
                command,
                reason,
                box_seq,
            },
            &mut std::io::stderr(),
        );
        if let Some(row) = row {
            self.fx
                .emit(super::super::tape::doors::LEDGER, || row.into_bytes());
        }
        // A typed row closes the ledger's wall too (`approvals::open_wall`):
        // the wall the act meets again is written again
        // (`Self::wall_closed_by_act`).
        if outcome == approvals::Outcome::Typed {
            self.wall_closed_by_act();
        }
        if let Some(host) = &self.stall_host
            && let Some(act) = crate::harness::acts::sort(rule_id, outcome, command).act
        {
            host.act_taken(act);
        }
    }

    /// The autocompact offer's stepper, bounded ([`StepperTrack`]): a
    /// `decision` to step it that reverses the last key on the same offer,
    /// repeats a key that did not move the size (the list's end), or would
    /// be past [`MAX_STEPPER_KEYS`], is escalated instead — the target is not
    /// among the sizes this offer steps through. Any other read (another
    /// box, or the offer gone) forgets the offer.
    fn stepper_check(
        &mut self,
        reading: &aterm_phase::Reading,
        rows: &[String],
        target: Option<u32>,
        decision: Decision,
    ) -> Decision {
        let track = &mut self.stepper;
        track.deciding = None;
        let Some((prompt, offer)) = reading
            .prompt
            .as_ref()
            .and_then(|p| p.autocompact_offer().map(|o| (p, o)))
        else {
            *track = StepperTrack::default();
            return decision;
        };
        let title = aterm_phase::prompt::title_row(prompt, rows).unwrap_or_default();
        let this = format!("{}\n{}", title.trim(), prompt.options[offer.keep].label);
        if track.offer.as_deref() != Some(this.as_str()) {
            *track = StepperTrack {
                offer: Some(this),
                ..StepperTrack::default()
            };
        }
        let Decision::Approve {
            choice: Choice::FocusKey { key, .. },
            rule_id,
            ..
        } = &decision
        else {
            return decision;
        };
        if *rule_id != RULE_AUTOCOMPACT_OFFER {
            return decision;
        }
        let key = *key;
        let ended = match track.last {
            Some((last, _)) if last != key => Some("the stepper turned back"),
            Some((_, at)) if at == offer.size => Some("the stepper did not move past it"),
            _ if track.keys >= MAX_STEPPER_KEYS => Some("the stepper never reached it"),
            _ => None,
        };
        if let Some(why) = ended {
            let target = target.map_or_else(|| "?".to_string(), |t| format!("{t}k"));
            return Decision::Escalate {
                reason: format!(
                    "an autocompact offer, and autocompact_offer = {target} is not among the \
                     sizes this offer steps through ({why} at {}k)",
                    offer.size
                ),
            };
        }
        track.deciding = Some((key, offer.size));
        decision
    }

    /// A stepper key decided by [`Self::stepper_check`] was pressed: it is
    /// the offer's last key.
    fn stepper_pressed(&mut self) {
        if let Some(pressed) = self.stepper.deciding.take() {
            self.stepper.last = Some(pressed);
            self.stepper.keys += 1;
        }
    }

    /// How much longer the box on `turn` must show before it is judged
    /// ([`BOX_SETTLE`]: the vendor refuses a key sooner, and re-arms its
    /// refusal on it), counted from the read that FIRST showed it — `None`
    /// once it has shown that long. A box seen before (held for a person,
    /// judged again after a moved screen) waits no more; one REDRAWN is the
    /// box seen before — its review key ([`Session::review_key`]) holds across
    /// a re-wrap, a moved focus and a ticking countdown, so none of them
    /// restarts the settle, and a box whose countdown ticks every second is
    /// never starved of its judgement. That key no longer tells the same box
    /// asked again after the worker worked from the one before it wherever
    /// the letters right above the two read alike (a whole box's key keeps
    /// no more of the transcript than those: `run.rs`'s `box_key`), so the
    /// box's MOUNT is ended where it is seen to end: a read with no box on it
    /// ([`Session::await_turn_from`]) and a press's box seen leaving
    /// ([`Self::box_left`]) forget it, and the box after it settles afresh.
    /// Claude Code's autocompact offer waits the setup's autocompact settle
    /// instead, the longer key guard of that dialog.
    fn box_young(&mut self, turn: &Turn, kind: PromptKind) -> Option<Duration> {
        let key = self.review_key(turn);
        let first = match &self.box_first_seen {
            Some((k, at)) if *k == key => *at,
            _ => {
                let now = self.fx.mono();
                self.box_first_seen = Some((key, now));
                // A new mount: a reset passed over at the one before it is
                // asked about afresh, and its remembered transcript candidate
                // is spent ([`Self::box_call`]), though no read saw it leave
                // (its press refused, a person's hold).
                self.reset_passed = None;
                self.reset_again = 0;
                self.spend_proved();
                now
            }
        };
        let settle = if kind == PromptKind::AutocompactOffer {
            self.setup.box_settle.max(self.setup.autocompact_settle)
        } else {
            self.setup.box_settle
        };
        let wait = settle.saturating_sub(self.fx.since(first));
        (!wait.is_zero()).then_some(wait)
    }

    /// Whether a person holds the act this look was about to take (`act`,
    /// for the journal): they typed into the session within `human_grace_s`
    /// of the last status read, made now when `fresh` ([`Self::
    /// foreground_program`]). Held, nothing is pressed, typed or escalated,
    /// the loop notes `HELD`, and the point is looked at again once the grace
    /// has passed ([`Self::turn_end_now`]).
    pub(super) fn held_for_person(
        &mut self,
        seq: u64,
        act: &str,
        fresh: bool,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if fresh {
            self.program = self.foreground_program()?;
        }
        let Some(until) = self.hands_off_until(opts) else {
            return Ok(false);
        };
        let who = if self.driver {
            "another driver's hand is on the session"
        } else {
            "a person is typing"
        };
        review.note(&format!(
            "HELD seq={seq} {who}: hands off until {} ({act})",
            utc_stamp(
                u64::try_from(self.now_unix()).unwrap_or(0)
                    + until.saturating_duration_since(self.fx.mono()).as_secs()
            )
        ));
        // Tests may look again sooner (`Session::set_person_hold_wait`): what
        // that look finds is the next `status`'s. Every other run's setup has
        // none, and holds to the end of the grace.
        let until = self
            .setup
            .person_hold_wait
            .map_or(until, |wait| until.min(self.fx.mono() + wait));
        self.set_due(until, "a person is typing");
        self.held = true;
        Ok(true)
    }

    /// Until when the loop keeps its hands off the session: a person typed
    /// into it within `human_grace_s` of the last status read
    /// ([`Self::person`]). `None`: no person, or the grace has passed.
    pub(super) fn hands_off_until(&self, opts: &SuperviseOpts) -> Option<Instant> {
        let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
        self.person
            .and_then(|typed| typed.checked_add(grace))
            .filter(|until| *until > self.fx.mono())
    }

    /// THE LAUNCH: whether a person's grace still holds the folder-trust
    /// dialog `p` the policy approves as `decision`, judged on the read
    /// `screen` and the `status` the decision made. `claude⏎` in a new folder
    /// draws a dialog whose last gesture is that Enter: held for the grace,
    /// it stood two minutes on `No, exit`, where a person's own Enter quits.
    /// It is answered at once only when ALL hold:
    ///
    /// 1. the trust rule approves it by a focus move (`Choice::Focus`; a
    ///    numbered dialog's digit carries no person fence), and it is a
    ///    trust box;
    /// 2. no other driver's hand is on the session, and Autopilot is not off
    ///    by [`Self::autopilot_read`] in this pass, with no wait between that
    ///    read and the first key — the decision's bare `meta` or one more; a
    ///    refused `meta` keeps the word last read, as for every decision
    ///    about to act;
    /// 3. the judged read carries a person count `n` and the server's
    ///    `person` witness for it (`"launch"`, judged under the rows' lock
    ///    against that very count), whose word names the program whose
    ///    reader judged the box;
    /// 4. the decision's `status` says the same: `launch=person` at
    ///    `launch_seq=n`, `program=` that agent, and `input=clear` exactly —
    ///    a missing field, `-`, `stalled` or `stopped` hold; `pending` or
    ///    `typeahead` is looked at again ([`LaunchCall::Again`]);
    /// 5. the witness has answered no OTHER box: one launch, one dialog.
    ///
    /// Why that is safe is the server's (aterm-gui `launch_witness`, "The
    /// proof"): `n` was loaded in the terminal-lock hold that parsed the
    /// shell's signed command start, before any frame could show a byte the
    /// command wrote, so no gesture counted since can be a reaction to its
    /// dialog. Every key of the press is fenced `if-human=n`
    /// ([`Self::press_focused`]): a person who keys first gets the box
    /// ([`Press::Yielded`]) and their grace, and the next read says `since`.
    /// A publication age, a sweep interval or the loop's first sight proves
    /// nothing here (the audit of 2026-10-16). What no fence covers is a
    /// person's own key written after the loop's arrow: it lands on the moved
    /// focus.
    pub(super) fn launch_call(
        &mut self,
        screen: &Screen,
        p: &PromptV2,
        decision: &Decision,
        opts: &SuperviseOpts,
    ) -> Result<LaunchCall, Fail> {
        let trusts = matches!(
            decision,
            Decision::Approve {
                rule_id,
                choice: Choice::Focus { .. },
                ..
            } if matches!(*rule_id, RULE_TRUST_ANY | RULE_TRUST_DIALOG)
        );
        if !trusts || p.kind != PromptKind::Trust || self.driver {
            return Ok(LaunchCall::Held);
        }
        let (Some(n), Some(witness)) = (screen.human_seq, screen.launch.as_ref()) else {
            return Ok(LaunchCall::Held);
        };
        let agent = aterm_phase::program_of(&witness.word);
        let status = &self.launch_status;
        let named = status.program.as_deref().and_then(aterm_phase::program_of);
        let box_key = box_identity(&screen.rows).unwrap_or_else(|| p.command.clone());
        if witness.seq != n
            || agent.is_none()
            || agent != Some(self.reader(&screen.rows).program())
            || named != agent
            || status.person_seq != Some(n)
            || self
                .launch_used
                .as_ref()
                .is_some_and(|(id, used)| *id == witness.id && *used != box_key)
        {
            return Ok(LaunchCall::Held);
        }
        match status.input.as_deref() {
            Some("clear") => {}
            Some("pending" | "typeahead") => return Ok(LaunchCall::Again),
            _ => return Ok(LaunchCall::Held),
        }
        if self.autopilot_read(opts)? == Autopilot::Off {
            return Ok(LaunchCall::Held);
        }
        self.launch_used = Some((witness.id, box_key));
        Ok(LaunchCall::Answer(witness.clone()))
    }

    /// One turn under the approval policy. A box [`decide`] approves is
    /// pressed under its guard (noted, ledgered, and reported to `review`)
    /// and the loop looks again once the box has left. A review point is: a
    /// box it escalates (the reason kept, [`Session::box_reason`]), anything
    /// that is not a box, the same approval back after two presses, a press
    /// whose guard matched no row on this very screen, a box that did not
    /// move after the press, or a press withheld because the host cannot
    /// prove its guards. A press the server skipped (the box left, the fenced screen moved, a hold, `ERR busy
    /// …`/`ERR rate`) is followed by a fresh read and decision, never the
    /// same press again. On a lost connection the unanswered press is noted,
    /// never counted as an approval, and the loss is ridden out.
    pub(super) fn auto_read(
        &mut self,
        turn: Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        approved: &mut Vec<Approved>,
        deadline: Instant,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let box_on = (turn.phase == Phase::Prompt)
            .then(|| self.reader(&turn.screen.rows).prompt(&turn.screen.rows))
            .flatten();
        let Some(p) = box_on else {
            return Ok(Step::Review(turn));
        };
        let seq = turn.screen.seq;
        // Too young to judge: waited on, bounded by what is left of the
        // settle, and read again.
        if let Some(wait) = self.box_young(&turn, p.kind) {
            self.wait(&["seq", &seq.to_string()], wait)?;
            return Ok(Step::Again { settle: false });
        }
        self.autopilot.begin();
        let decision = self.decide_box(&turn.screen, opts, allow)?;
        #[cfg(test)]
        if PANIC_AT_DECIDE.with(std::cell::Cell::get) {
            panic!("a planted panic in the policy, at the decision on seq={seq}");
        }
        // The decision's mark on a harness tape: the screen it judged, the
        // rule and the outcome, before anything is done about it — a reader
        // asks "why did it press 2" of the mark, the act that follows and
        // the line that says it (`aterm drive tape show --decisions`).
        self.fx.decide(
            if p.kind == PromptKind::Question {
                super::super::tape::doors::DECIDE_QUESTION
            } else {
                super::super::tape::doors::DECIDE_APPROVAL
            },
            &turn.screen,
            match &decision {
                Decision::Approve { rule_id, .. } | Decision::Decline { rule_id, .. } => {
                    Some(*rule_id)
                }
                Decision::Escalate { .. } => None,
            },
            |_| format!("{decision:?}"),
            self.decided_on.as_ref(),
        );
        // The `status` that named the program says the worker is not reading
        // its input: this box is neither pressed nor escalated; the next look
        // holds (`stall.rs`).
        if self.stall_in_hand(review) {
            return Ok(Step::Again { settle: false });
        }
        // AUTOPILOT OFF, by the session's word as this decision read it (the
        // bare `meta` the decision made, or one more where the host keeps the
        // word): the box is neither pressed nor escalated — it is the
        // person's, as on any session nobody supervises — and the loop parks
        // until the word changes ([`Self::park_while_off`]), then reads and
        // decides the screen afresh. The harness's loops alone: a manager's
        // one look (`supervise`) is the manager's.
        if review.unattended() && self.autopilot_read(opts)? == Autopilot::Off {
            self.park_while_off(seq, deadline, opts, review)?;
            return Ok(Step::Again { settle: false });
        }
        // A person at the keyboard wins: an answer the policy has is held
        // until their grace has passed, on the status read the decision was
        // made on, and neither pressed nor escalated. On a question dialog the
        // loop has already written to, their input (or another driver's hand)
        // makes its end theirs (R1): after their grace the loop may still
        // press Enter on its own text, but it tells no `CHOSE` for it. THE
        // LAUNCH is the one answer that grace does not hold
        // ([`Self::launch_call`]).
        let answers = matches!(
            decision,
            Decision::Approve { .. } | Decision::Decline { .. }
        );
        let launch = if answers && self.hands_off_until(opts).is_some() {
            self.launch_call(&turn.screen, &p, &decision, opts)?
        } else {
            LaunchCall::Held
        };
        match launch {
            LaunchCall::Answer(w) => review.note(&format!(
                "LAUNCH seq={seq} id={} word={} the person's last gesture was the Enter that \
                 started it, none since (count {}): answered, not held",
                w.id, w.word, w.seq
            )),
            LaunchCall::Again => {
                self.wait(&["seq", &seq.to_string()], PROGRAM_SETTLE)?;
                return Ok(Step::Again { settle: false });
            }
            LaunchCall::Held => {
                if answers && self.held_for_person(seq, "the box's answer", false, opts, review)? {
                    if p.kind == PromptKind::Question {
                        self.question.person_gave_input();
                    }
                    return Ok(Step::Review(turn));
                }
            }
        }
        self.auto_act(turn, p, decision, opts, approved, deadline, review)
    }

    /// What [`Self::auto_read`] does with the box `p` on `turn` once it is
    /// decided, the stall check and the person's hold behind it: escalate
    /// it, or press its answer and see the press through. Its own function
    /// so that the usage-limit dialog's reset, passed over before its first
    /// key ([`Self::reset_take`]), is answered by the decision made without
    /// the reset's rule ([`Session::reset_fallback`]) through this same
    /// path, on the same read.
    #[allow(clippy::too_many_arguments)]
    fn auto_act(
        &mut self,
        turn: Turn,
        p: aterm_phase::PromptV2,
        decision: Decision,
        opts: &SuperviseOpts,
        approved: &mut Vec<Approved>,
        deadline: Instant,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let seq = turn.screen.seq;
        let notes = opts.notes.as_deref();
        let (rule_id, act, subject, unproven) = match decision {
            Decision::Escalate { reason } => {
                self.append_note(
                    notes,
                    &format!("handed to the manager ({reason}): {}", p.command),
                )?;
                self.ledger_row("-", approvals::Outcome::Escalated, &p.command, &reason, seq);
                if p.kind == PromptKind::Question {
                    note_dialog(review, seq, "-", &turn.screen.rows);
                    // Handed to a person for a reason of its own, not the
                    // policy holding it (`question_held`), which a hand-back
                    // lifts: answered then, the answer is still the policy's.
                    if !self.question_held {
                        self.question.theirs = true;
                    }
                }
                self.box_reason = Some((self.review_key(&turn), reason));
                return Ok(Step::Review(turn));
            }
            Decision::Approve {
                rule_id,
                guard,
                choice,
                subject,
                unproven,
            } => (rule_id, Act::Approve { guard, choice }, subject, unproven),
            Decision::Decline {
                rule_id,
                refusal,
                text,
                subject,
            } => (rule_id, Act::Decline { refusal, text }, subject, None),
        };
        // What the lines name (module header): the decision's subject.
        let what = shown_subject(&subject, &p.command);
        // A question's answer: its dialog read again from the same rows (the
        // key's bookkeeping needs it), and the waits the pure decision cannot
        // know (module docs, "A question dialog").
        let (question, toggle) = match &act {
            Act::Approve {
                choice: Choice::Answer(answer),
                ..
            } => {
                let Some(dialog) =
                    aterm_phase::parse_prompt_v2(&turn.screen.rows).and_then(|v| v.question_dialog)
                else {
                    let why = "the question dialog was not read again from the same rows";
                    self.question.theirs = true;
                    self.append_note(notes, &format!("handed to the manager ({why}): {what}"))?;
                    self.ledger_row(rule_id, approvals::Outcome::Escalated, &what, why, seq);
                    self.box_reason = Some((self.review_key(&turn), why.to_string()));
                    return Ok(Step::Review(turn));
                };
                let identity = box_identity(&turn.screen.rows).unwrap_or_else(|| what.clone());
                // An Enter that moves on or submits answers the question; a
                // multi-select toggle and a written policy text do not. Only
                // the answering Enter is counted as an answer
                // (`Approved::question`, below).
                let answering = answer
                    .enter_target()
                    .is_some_and(|target| answers(&dialog, target));
                let this_question = question_key(&dialog, &turn.screen.rows);
                let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
                // The re-ask pause (O5) is waited before the keys of the
                // answering Enter, and, under a text word, before any key of an
                // answer (its moves, its policy text, a falling-back tab's
                // toggle), so the dialog waits it out with nothing of the
                // loop's in it.
                let text_word = self.question.deciding.is_some_and(|w| w.text().is_some());
                let reask = (answering || text_word).then_some(this_question.as_str());
                match self.question_hold(
                    &turn, &dialog, &identity, reask, approved, grace, rule_id, review,
                )? {
                    None => {}
                    Some(Hold::Again) => return Ok(Step::Again { settle: true }),
                    Some(Hold::BackOff(why)) => {
                        return self.box_back_off(&turn, &what, &why, review);
                    }
                    Some(Hold::HandOver(why)) => {
                        self.question.theirs = true;
                        self.append_note(notes, &format!("handed to the manager ({why}): {what}"))?;
                        self.ledger_row(rule_id, approvals::Outcome::Escalated, &what, &why, seq);
                        note_dialog(review, seq, rule_id, &turn.screen.rows);
                        self.box_reason = Some((self.review_key(&turn), why));
                        return Ok(Step::Review(turn));
                    }
                }
                let toggle = match (&dialog.form, answer.enter_target()) {
                    (QuestionForm::Multi { .. }, Some(AnswerTarget::Option(n))) => {
                        Some((this_question.clone(), n))
                    }
                    _ => None,
                };
                (answering.then_some(this_question), toggle)
            }
            _ => (None, None),
        };
        let answer_named = question.is_some();
        // A decline's box as the decline knows it: the same across its own
        // keystrokes, and a box too tall to read has no command.
        let declined = match &act {
            Act::Decline { .. } => {
                let key = super::press::decline_box(&p, &turn.screen.rows);
                if let Some(pause) = self.decline_again(&key, approved) {
                    review.note(&format!(
                        "WAITING seq={seq} rule={rule_id} the same box came back after its \
                         declines since the last review point (the worker has not acted on its \
                         reason yet): declined again in {} ms",
                        pause.as_millis()
                    ));
                    self.wait(&["seq", &seq.to_string()], pause)?;
                    return Ok(Step::Again { settle: true });
                }
                Some(key)
            }
            Act::Approve { .. } => None,
        };
        // Under full power a box that comes back is answered again (module
        // docs): the cap is the safe rules', on the parser's command; a
        // question's has its own ([`Self::question_hold`]), as does a
        // decline's ([`Self::decline_again`]).
        let repeats = approved
            .iter()
            .filter(|a| a.declined.is_none() && a.command == p.command)
            .count();
        if opts.policy.approve != Approve::All
            && !is_question_rule(rule_id)
            && declined.is_none()
            && repeats >= MAX_APPROVALS_OF_ONE_COMMAND
        {
            let why = "the same prompt came back after two approvals";
            self.append_note(notes, &format!("handed to the manager ({why}): {what}"))?;
            self.ledger_row(rule_id, approvals::Outcome::Escalated, &what, why, seq);
            self.box_reason = Some((self.review_key(&turn), why.to_string()));
            return Ok(Step::Review(turn));
        }
        // THE USAGE-LIMIT DIALOG'S GRANT ROW: the loop's own gates and then
        // the host's word, LAST, before any key ([`Self::reset_take`]). A
        // pass answers the dialog with the decision made without the reset's
        // rule on this same read — the wait row, by the keys a build with
        // `limit_reset` off writes.
        let reset = rule_id == RULE_LIMIT_RESET;
        if reset {
            match self.reset_take(&turn, opts, review)? {
                super::reset_loop::ResetAsked::Take => {}
                super::reset_loop::ResetAsked::Again => return Ok(Step::Again { settle: false }),
                super::reset_loop::ResetAsked::Pass => {
                    let plain = self
                        .reset_fallback
                        .take()
                        .unwrap_or_else(|| Decision::Escalate {
                            reason: "a usage-limit dialog whose reset was passed over".to_string(),
                        });
                    return self.auto_act(turn, p, plain, opts, approved, deadline, review);
                }
            }
        }
        // A press no safe rule proved says it was waved through, and why; a
        // decline, the text the worker was given; the grant row, the wall its
        // claim is for.
        let unproven = unproven.map(|u| format!("unproven: {u}"));
        let over = match (&act, &self.reset_taken) {
            (Act::Decline { text, .. }, _) => text.clone(),
            (Act::Approve { .. }, Some((go, _))) if reset => {
                format!("taken: wall until {}", go.until)
            }
            (Act::Approve { .. }, _) => unproven.clone().unwrap_or_default(),
        };
        let approved_note = match (&act, &unproven) {
            (Act::Decline { text, .. }, _) => format!("declined ({rule_id}): {what} — {text}"),
            (Act::Approve { .. }, Some(u)) => format!("approved ({rule_id}; {u}): {what}"),
            (Act::Approve { .. }, None) if rule_id == RULE_READ_ONLY => {
                format!("approved read-only: {what}")
            }
            (Act::Approve { .. }, None) => format!("approved ({rule_id}): {what}"),
        };
        let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
        let pressing = match &act {
            Act::Approve {
                guard,
                choice: Choice::Digit(n),
            } => self.press_one_guarded(guard, &n.to_string(), &turn.screen)?,
            Act::Approve {
                guard,
                choice: Choice::Focus { steps, label },
            } => {
                // Fence every folder-trust key on the judged read's person
                // count too. A person may key between the read and the arrow
                // or Enter, after their grace or under THE LAUNCH
                // ([`Self::launch_call`]), whose witness is that same count.
                // The grant row retains the same count fence. A person's own
                // key after an accepted arrow can still land on the new focus.
                // The autocompact offer's keys too: each one a person keyed
                // past is theirs.
                let person = turn.screen.human_seq.filter(|_| {
                    reset
                        || p.kind == PromptKind::Trust
                        || p.kind == PromptKind::AutocompactOffer
                        || is_muse(&self.reader(&turn.screen.rows).program())
                });
                self.press_focused(&p, *steps, label, "enter", guard, &turn.screen, person)?
            }
            Act::Approve {
                guard,
                choice: Choice::FocusKey { steps, label, key },
            } => {
                let person = turn
                    .screen
                    .human_seq
                    .filter(|_| p.kind == PromptKind::AutocompactOffer);
                self.press_focused(&p, *steps, label, key, guard, &turn.screen, person)?
            }
            Act::Approve {
                choice: Choice::Answer(answer),
                ..
            } => self.press_question(answer, &turn.screen, grace)?,
            Act::Approve {
                choice: Choice::Note { text },
                ..
            } => self.press_codex_note(*text, &turn.screen, grace)?,
            Act::Decline { refusal, text } => {
                self.press_decline(*refusal, text, &turn.screen, grace)?
            }
        };
        // The policy text's read-back, when the press made one the screen
        // held still before (`Press::Typed` below): taken here, whatever the
        // press did, so none outlives it.
        let read_back = self.read_back.take();
        // The grant row's approval is named by its own label, so the safe
        // rules' cap never counts it against the wait row of the same dialog.
        let this = Approved {
            command: if reset {
                what.clone()
            } else {
                p.command.clone()
            },
            question,
            declined,
        };
        let press = match pressing {
            Pressing::Done(press) => press,
            Pressing::Unconfirmed { why, retry } => {
                if retry && self.retries_presses(opts, review, rule_id) {
                    return self.press_missed(&turn, &what, &why, opts, review);
                }
                // A grant row that cannot be pressed is passed over, never
                // handed to a person: the wait row answers its dialog.
                if reset {
                    self.reset_pass(
                        seq,
                        &format!("the row was not pressed ({why})"),
                        opts,
                        review,
                    );
                    return Ok(Step::Again { settle: false });
                }
                let why = if this.declined.is_some() {
                    format!("the decline could not be carried out: {why}")
                } else {
                    why
                };
                self.append_note(notes, &format!("handed to the manager ({why}): {}", what))?;
                self.ledger_row(rule_id, approvals::Outcome::Escalated, &what, &why, seq);
                if is_question_rule(rule_id) {
                    self.question.theirs = true;
                    note_dialog(review, seq, rule_id, &turn.screen.rows);
                }
                self.box_reason = Some((self.review_key(&turn), why));
                return Ok(Step::Review(turn));
            }
            Pressing::Halted { why } => {
                self.ledger_row(rule_id, approvals::Outcome::Deferred, &what, &why, seq);
                self.park_on_hold(&why, seq, review)?;
                return Ok(Step::Again { settle: false });
            }
            Pressing::Refused { why } => {
                self.ledger_row(rule_id, approvals::Outcome::Deferred, &what, &why, seq);
                let anchor = match &act {
                    Act::Approve { guard, .. } => Some(guard.as_str()),
                    Act::Decline { .. } => None,
                };
                self.back_off(&why, seq, anchor, review)?;
                return Ok(Step::Again { settle: false });
            }
            Pressing::Lost { why } => {
                // The grant row's Enter with no answer: it may have taken
                // the row, so what it opens is owed its end — and never
                // entered ([`Self::reset_row_lost`]). Lost at an arrow, no
                // row was taken: the claim stays in hand, and the dialog is
                // decided again after the reconnect.
                if reset && self.focus_key_tried {
                    self.reset_row_lost(&what, seq);
                }
                self.append_note(
                    notes,
                    &format!(
                        "pressed, no answer came; the box is read again after the reconnect: \
                         {what}"
                    ),
                )?;
                return Err(Fail::Lost(why));
            }
        };
        self.refusal_pause = REFUSAL_PAUSE;
        match press {
            // A question's key: its effect is the next read's to show (the
            // next tab, a checkbox, the focus, the review gone; R3a), never
            // waited out as "the box must leave", which any redraw satisfies.
            Press::Pressed { seq: at } if is_question_rule(rule_id) => {
                self.changed_streak = 0;
                // An answer the loop's own Enter gave (not a toggle, not the
                // review's submit): told once when the dialog is done; a retry
                // of the same Enter is told once with it.
                let submit = matches!(
                    &act,
                    Act::Approve {
                        choice: Choice::Answer(answer),
                        ..
                    } if answer.enter_target() == Some(AnswerTarget::ReviewSubmit)
                );
                if answer_named && !submit && self.question.answered.last() != Some(&what) {
                    self.question.answered.push(what.clone());
                }
                // A question read by its answers' roles (Codex's, agy's and
                // Gemini CLI's by a digit, Muse Code's by the cursor and
                // Enter, Codex's and Muse Code's note by its Enter): the key
                // written answered it, so it is told as a dialog's answer is,
                // by its question. A note is the latched word's text: told
                // under that word.
                if let Some((told, note)) = told_by_role(&act, &p, &what)
                    && self.question.answered.last() != Some(&told)
                {
                    self.question.answered.push(told);
                    self.question.by_text |= note;
                }
                // Answered by the worker from the loop's own text in its
                // field (OwnText, or the multi-select's button after it): the
                // dialog is told under the latched word.
                if answer_named
                    && !submit
                    && this
                        .question
                        .as_ref()
                        .is_some_and(|q| self.question.typed.contains_key(q))
                {
                    self.question.by_text = true;
                }
                // A multi-select toggle the loop made: its check is the
                // loop's, not a person's (R2 (d)).
                if let Some((q, n)) = toggle {
                    self.question.toggled.entry(q).or_default().insert(n);
                }
                approved.push(this);
                self.append_note(notes, &approved_note)?;
                self.ledger_row(rule_id, approvals::Outcome::Approved, &what, &over, seq);
                review.approved(at, &what)?;
                note_unproven(review, at, rule_id, unproven.as_deref());
                note_dialog(review, seq, rule_id, &turn.screen.rows);
                // Let the worker take the key before the next read: the
                // first content change after it, bounded.
                self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
                Ok(Step::Again { settle: true })
            }
            // The usage-limit dialog's grant row, taken: what it opens is
            // the loop's own step, owed from the Enter on and run from here
            // ([`Self::reset_run`]) — the pending line, the confirmation and
            // its end are never another rule's to judge.
            Press::Pressed { seq: at } if reset => {
                self.changed_streak = 0;
                self.press_retry = None;
                self.reset_row_taken();
                approved.push(this);
                self.answered(&act, rule_id, &what, &over, (at, seq), review)?;
                self.append_note(notes, &approved_note)?;
                self.box_gone();
                self.wait(&["seq", &at.to_string()], self.setup.reset_waits.step)?;
                self.reset_run(None, deadline, opts, review)?;
                Ok(Step::Again { settle: true })
            }
            // The policy text written into the free-text row and read back
            // exactly: no Enter here; the next read decides it (Enter on the
            // field, or the multi-select's button), under R1 and R3a like any
            // key. Not an answer yet: nothing is added to what `CHOSE` tells.
            Press::Typed { seq: at } => {
                self.changed_streak = 0;
                let word = match &act {
                    Act::Approve {
                        choice: Choice::Answer(answer),
                        ..
                    } => answer.typed().unwrap_or_default(),
                    _ => QuestionPriority::default(),
                }
                .word();
                approved.push(this);
                self.append_note(notes, &approved_note)?;
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Approved,
                    &what,
                    &format!(
                        "typed the {word} policy text into the free-text row: the worker decides"
                    ),
                    seq,
                );
                review.approved(at, &what)?;
                note_unproven(review, at, rule_id, unproven.as_deref());
                note_dialog(review, seq, rule_id, &turn.screen.rows);
                // No wait on `at`: the press waited on the send's own seq and
                // read the text back settled, so a wait past the read-back's
                // seq would wait on a change that is not coming. That read,
                // made after the screen held still, is the next look's turn,
                // saving a settle and a second read before the Enter. Its box
                // is past the vendor's key guard (it held still longer than
                // [`BOX_SETTLE`] before the read), so the settle is counted
                // from before it. A read-back the screen did not hold still
                // before gets a fresh look.
                match read_back
                    .map(|(back, still)| (self.turn_of(back), still))
                    .filter(|(next, _)| next.phase == Phase::Prompt)
                {
                    Some((next, still)) => {
                        let since = self
                            .fx
                            .mono()
                            .checked_sub(still)
                            .unwrap_or_else(|| self.fx.mono());
                        self.box_first_seen = Some((self.review_key(&next), since));
                        Ok(Step::Decide(next.screen))
                    }
                    None => Ok(Step::Again { settle: true }),
                }
            }
            Press::Yielded { seq: at } => {
                if is_question_rule(rule_id) {
                    self.question.theirs = true;
                }
                // A person keyed the session while the answer was keyed:
                // nothing more was sent; the next read waits for their quiet
                // (`question_hold`) and decides again, never escalated for the
                // wait (R1).
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &what,
                    "a person gave the session input while the answer was keyed: decided again \
                     after their quiet",
                    at,
                );
                Ok(Step::Again { settle: true })
            }
            Press::Unseen { seq: at } => {
                // A focus move written, its effect not seen yet: nothing is
                // re-sent here; the next read decides (R3a).
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &what,
                    "the dialog has not shown the key's effect yet",
                    at,
                );
                Ok(Step::Again { settle: true })
            }
            Press::Pressed { seq: at } => {
                self.changed_streak = 0;
                if matches!(
                    &act,
                    Act::Approve {
                        choice: Choice::FocusKey { .. },
                        ..
                    }
                ) {
                    self.stepper_pressed();
                }
                approved.push(this);
                if let Err(e) = self.append_note(notes, &approved_note) {
                    return Err(e.into());
                }
                self.answered(&act, rule_id, &what, &over, (at, seq), review)?;
                note_unproven(review, at, rule_id, unproven.as_deref());
                // The box must leave before the next look: `await gone` is
                // level-triggered and a box shows no busy footer, so an
                // immediate re-read of an unchanged screen would match the
                // same box and press it again. Any move is not its leaving:
                // the vendor redraws a box it has taken (the countdown wiped,
                // the transcript reflowed) before it takes it down
                // ([`box_identity`]).
                let identity = box_identity(&turn.screen.rows).unwrap_or_default();
                let screen = match self.box_left(at, &identity, deadline)? {
                    Past::Moved => {
                        self.press_retry = None;
                        return Ok(Step::Again { settle: true });
                    }
                    Past::Still(Some(screen)) => screen,
                    // The budget ran out waiting: nothing is read or handed
                    // over after it; the box as last read is the timeout's
                    // ([`Self::look`]).
                    Past::Still(None) => return Ok(Step::Review(turn)),
                };
                let why = "the box did not change after the press";
                let still = self.turn_of(screen);
                if self.retries_presses(opts, review, rule_id) {
                    return self.press_missed(&still, &what, why, opts, review);
                }
                self.append_note(notes, &format!("handed to the manager ({why}): {what}"))?;
                Ok(Step::Review(still))
            }
            Press::Changed { seq: at } => {
                // The screen moved between the read and the press: read and
                // decide again, and after a streak of these, hand the box over
                // rather than press it unfenced ([`MAX_CHANGED_PRESSES`]).
                self.changed_streak += 1;
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &what,
                    "the screen moved past the judged read",
                    at,
                );
                if self.changed_streak >= MAX_CHANGED_PRESSES {
                    self.changed_streak = 0;
                    let why = format!(
                        "{MAX_CHANGED_PRESSES} fenced presses in a row did not land (the screen \
                         moved, or the key was not taken)"
                    );
                    if self.retries_presses(opts, review, rule_id) {
                        return self.press_missed(&turn, &what, &why, opts, review);
                    }
                    self.append_note(notes, &format!("handed to the manager ({why}): {}", what))?;
                    self.box_reason = Some((self.review_key(&turn), why));
                    return Ok(Step::Review(turn));
                }
                // A question's key skipped this way settles and is decided
                // again (a person's key may be what moved the screen, and the
                // quiet is checked again first), never pressed again at once
                // (R2).
                Ok(Step::Again {
                    settle: is_question_rule(rule_id),
                })
            }
            Press::Skipped { seq: at } => {
                self.changed_streak = 0;
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &what,
                    "the guarded row was not on the screen",
                    at,
                );
                if at == seq {
                    // The very screen we parsed, and the guard found no row:
                    // under the safe rules this box is not one the supervisor
                    // answers; at full power it is read and tried again.
                    let why = "aterm could not find the box's row";
                    if self.retries_presses(opts, review, rule_id) {
                        return self.press_missed(&turn, &what, why, opts, review);
                    }
                    self.append_note(notes, &format!("handed to the manager ({why}): {}", what))?;
                    self.box_reason = Some((self.review_key(&turn), why.to_string()));
                    return Ok(Step::Review(turn));
                }
                self.append_note(
                    notes,
                    &format!("nothing pressed, the box had left the screen: {what}"),
                )?;
                Ok(Step::Again { settle: false })
            }
        }
    }
}

/// The longest pause between tries of a press that did not land at full
/// power ([`Session::press_missed`]); the first is [`REFUSAL_PAUSE`], each
/// next twice the last.
const PRESS_RETRY_MAX: Duration = Duration::from_secs(60);
/// How long a box whose presses keep missing is tried before the session is
/// badged — the tries going on behind the badge.
pub(super) const PRESS_BADGE_AFTER: Duration = Duration::from_secs(120);

/// A box whose press did not land, being tried again at full power
/// ([`Session::press_missed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PressRetry {
    /// The box ([`box_identity`]).
    key: String,
    tries: u32,
    since: Instant,
    badged: bool,
}

impl<C: Ctl> Session<'_, C> {
    /// Whether a press that did not land is tried again
    /// ([`Self::press_missed`]) rather than handed over: with nobody there to
    /// take it (`watch`, the window's host; `supervise`'s one look hands it to
    /// its manager) at full power, or for a question's answer or the
    /// usage-limit dialog's wait or reset (`rule_id`), which no `approve`
    /// level limits.
    fn retries_presses(&self, opts: &SuperviseOpts, review: &dyn Review, rule_id: &str) -> bool {
        review.unattended()
            && (opts.policy.approve == Approve::All
                || is_question_rule(rule_id)
                || rule_id == RULE_LIMIT_WAIT
                || rule_id == RULE_LIMIT_RESET
                || rule_id == RULE_AUTOCOMPACT_OFFER)
    }

    /// Full power's answer to a press that did not land (the box did not
    /// change after the press, a streak of fenced presses that did not land,
    /// a focus move that did not land, or a guard that matched no row on the
    /// very screen read): [`Self::box_back_off`], journaled `WAITING seq=<n>
    /// the press did not land (<why>) …`, since nobody else can answer it.
    ///
    /// The usage-limit dialog's GRANT ROW is tried again on the same pauses
    /// and NEVER BADGED ([`Self::reset_press_missed`]): a reset raises no
    /// attention, and its dialog has another answer.
    fn press_missed(
        &mut self,
        turn: &Turn,
        what: &str,
        why: &str,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let why = format!("the press did not land ({why})");
        if self.reset_taken.is_some() {
            return self.reset_press_missed(turn, &why, opts, review);
        }
        self.box_back_off(turn, what, &why, review)
    }

    /// The grant row's press did not land, the host's go for it in hand
    /// ([`Session::reset_taken`]): read and tried again after the press
    /// back-off's pause — the host answers its own claim with the go again —
    /// and, where any other box would be badged ([`PRESS_BADGE_AFTER`]),
    /// PASSED OVER instead ([`Self::reset_pass`]: the host is told the reset
    /// was kept, nothing opened), so the wait row answers the dialog.
    fn reset_press_missed(
        &mut self,
        turn: &Turn,
        why: &str,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let key = box_identity(&turn.screen.rows).unwrap_or_default();
        let (now, seq) = (self.fx.mono(), turn.screen.seq);
        let (tries, since) = match self.press_retry.as_mut().filter(|r| r.key == key) {
            Some(r) => {
                r.tries = r.tries.saturating_add(1);
                (r.tries, r.since)
            }
            None => {
                self.press_retry = Some(PressRetry {
                    key,
                    tries: 1,
                    since: now,
                    badged: false,
                });
                (1, now)
            }
        };
        if now.duration_since(since) >= self.setup.press_badge_after {
            self.press_retry = None;
            self.reset_pass(seq, why, opts, review);
            return Ok(Step::Again { settle: false });
        }
        let pause = crate::supervise::ladder::doubling(REFUSAL_PAUSE, tries - 1, PRESS_RETRY_MAX);
        review.note(&format!(
            "WAITING seq={seq} rule={RULE_LIMIT_RESET} {why} (try {tries}): read and tried again \
             in {} ms",
            pause.as_millis()
        ));
        self.wait(&["seq", &seq.to_string()], pause)?;
        Ok(Step::Again { settle: false })
    }

    /// The press back-off, for a box nobody else is there to answer (a press
    /// that did not land, [`Self::press_missed`]; a question key the dialog
    /// did not take; a screen that never held still for the key to be sent
    /// again, [`Self::question_hold`]): the box is read and decided again
    /// after a pause that doubles from [`REFUSAL_PAUSE`] to
    /// [`PRESS_RETRY_MAX`] (Claude Code drops keys 0.3 s after it draws a
    /// box), and once it has missed for [`PRESS_BADGE_AFTER`] the session is
    /// badged ([`Self::escalate_point`]) while the tries go on. Journaled
    /// `WAITING seq=<n> <why> (try <n>) …` each time.
    fn box_back_off(
        &mut self,
        turn: &Turn,
        what: &str,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let key = box_identity(&turn.screen.rows).unwrap_or_default();
        let now = self.fx.mono();
        if self.press_retry.as_ref().is_none_or(|r| r.key != key) {
            self.press_retry = Some(PressRetry {
                key,
                tries: 0,
                since: now,
                badged: false,
            });
        }
        let Some(r) = self.press_retry.as_mut() else {
            return Ok(Step::Again { settle: false });
        };
        r.tries = r.tries.saturating_add(1);
        let pause = crate::supervise::ladder::doubling(REFUSAL_PAUSE, r.tries - 1, PRESS_RETRY_MAX);
        let badge = !r.badged && now.duration_since(r.since) >= self.setup.press_badge_after;
        r.badged |= badge;
        let tries = r.tries;
        let seq = turn.screen.seq;
        review.note(&format!(
            "WAITING seq={seq} {why} (try {tries}): read and tried again in {} ms: {}",
            pause.as_millis(),
            clip(what)
        ));
        if badge && review.unattended() {
            let reason = format!("{why}; still trying ({tries} tries)");
            self.escalate_point(turn, Some(&reason), review)?;
        }
        self.wait(&["seq", &seq.to_string()], pause)?;
        Ok(Step::Again { settle: false })
    }

    /// What a question's answer waits on before it is keyed (module docs, "A
    /// question dialog"): `None` to key it now, [`Hold::Again`] after a wait
    /// (a person not quiet for `grace` yet, the loop's own last key not seen
    /// taking effect yet), or [`Hold::HandOver`]. `identity` is the box as
    /// drawn ([`box_identity`]); `answering` the question this decision's
    /// keys begin or answer ([`question_key`]; `None` for a multi-select
    /// toggle under `recommended`), for the re-ask pause; `rule_id` the
    /// decision's rule, which a `WAITING` line names.
    #[allow(clippy::too_many_arguments)]
    fn question_hold(
        &mut self,
        turn: &Turn,
        dialog: &QuestionDialog,
        identity: &str,
        answering: Option<&str>,
        approved: &[Approved],
        grace: Duration,
        rule_id: &str,
        review: &mut dyn Review,
    ) -> Result<Option<Hold>, Fail> {
        let seq = turn.screen.seq;
        let quiet_ms = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX);
        // 1. A person has been quiet (R1).
        match turn.screen.human {
            HumanInput::Never => {}
            HumanInput::Ago(ms) if ms >= quiet_ms => {}
            HumanInput::Ago(ms) => {
                // A person's input after the loop's own keys, or its policy
                // text, reached the dialog: its end is theirs
                // ([`QuestionMemory::person_gave_input`]).
                self.question.person_gave_input();
                self.note_question_wait(
                    review,
                    seq,
                    rule_id,
                    identity,
                    &format!(
                        "a person gave the session input {ms} ms ago: the question waits for {} \
                         s of their quiet",
                        grace.as_secs()
                    ),
                );
                // Woken early by any change: the next read re-checks.
                let left = Duration::from_millis(quiet_ms - ms);
                self.wait(&["seq", &seq.to_string()], left.min(WAIT_STEP))?;
                return Ok(Some(Hold::Again));
            }
            HumanInput::Unknown if self.question.standin.as_deref() == Some(identity) => {}
            HumanInput::Unknown => {
                self.note_question_wait(
                    review,
                    seq,
                    rule_id,
                    identity,
                    &format!(
                        "this host does not stamp a person's input: the question waits for the \
                         screen to hold still {} s",
                        QUESTION_STANDIN_QUIET.as_secs()
                    ),
                );
                let ms = QUESTION_STANDIN_QUIET.as_millis().to_string();
                match self.wait(&["idle", &ms], QUESTION_STANDIN_QUIET + SETTLE_CAP)? {
                    Wait::Latched | Wait::Unsupported => {
                        self.question.standin = Some(identity.to_string());
                    }
                    Wait::TimedOut => {}
                }
                return Ok(Some(Hold::Again));
            }
        }
        // 2. The loop's own last key took effect (R3a).
        let focus = dialog.focus();
        let unchanged = self
            .question
            .pending
            .clone()
            .filter(|p| p.identity == identity && p.focus == focus);
        // The multi-select toggle still in flight on this very box, if any:
        // its check may not be drawn yet (R2 (d) below).
        let toggling = match &unchanged {
            Some(QuestionPending {
                kind: PendingKind::Enter,
                focus: QuestionFocus::Option(n),
                ..
            }) => Some(*n),
            _ => None,
        };
        // Nobody to hand a key the dialog did not take to (module header,
        // 2): it is tried again on the press back-off instead.
        let unattended = review.unattended();
        // A key sent again on the box as it was keyed is no re-ask (3).
        let retrying = unchanged.is_some();
        // (A policy text not taken never gets here: `decide_box` hands that
        // box over first, since a text is never written twice.)
        match unchanged {
            Some(QuestionPending {
                waited: false,
                unsettled,
                ..
            }) => {
                if unsettled == 0 {
                    review.note(&format!(
                        "WAITING seq={seq} rule={rule_id} the dialog has not shown the last \
                         key's effect: read again once the screen holds still"
                    ));
                }
                // Only a screen that held still is the retry's premise (a key
                // read by Claude Code by then); a wait that ran out is waited
                // again, and after a few backed off (or, to a manager, handed
                // over), never keyed.
                let settled = matches!(self.wait(&["idle", IDLE_MS], WAIT_STEP)?, Wait::Latched);
                if let Some(p) = self.question.pending.as_mut() {
                    if settled {
                        p.waited = true;
                    } else {
                        p.unsettled += 1;
                        if p.unsettled >= MAX_UNSETTLED_WAITS {
                            if unattended {
                                p.unsettled = 0;
                                return Ok(Some(Hold::BackOff(
                                    "the screen has not held still for the key to be sent again"
                                        .to_string(),
                                )));
                            }
                            self.question.pending = None;
                            return Ok(Some(Hold::HandOver(
                                "the dialog did not take the key, and the screen never held \
                                 still after it for the key to be sent once more"
                                    .to_string(),
                            )));
                        }
                    }
                }
                return Ok(Some(Hold::Again));
            }
            Some(QuestionPending {
                retries,
                backed_off,
                ..
            }) if retries > 0 => {
                if !unattended {
                    self.question.pending = None;
                    return Ok(Some(Hold::HandOver(
                        "the dialog did not take the key: nothing changed after it, nor after it \
                         was sent once more"
                            .to_string(),
                    )));
                }
                if !backed_off {
                    if let Some(p) = self.question.pending.as_mut() {
                        p.backed_off = true;
                    }
                    return Ok(Some(Hold::BackOff(format!(
                        "the dialog took none of the {} keys sent",
                        retries.saturating_add(1)
                    ))));
                }
                // The back-off waited, the screen still: the key again.
            }
            // Once more at once: a key the dialog refused (its typeahead
            // window, O4) is gone, and the screen held still after it. A retry
            // is counted when that key is written (`question_pending`), so a
            // press that sends nothing does not count as one.
            Some(_) => {}
            None => {
                // The last key took effect: its box's back-off is over.
                if let Some(p) = self.question.pending.take()
                    && self
                        .press_retry
                        .as_ref()
                        .is_some_and(|r| r.key == p.identity)
                {
                    self.press_retry = None;
                }
            }
        }
        // R2 (d): on a multi-select, every check is one the loop made, and
        // every check it made still reads checked; else a person has begun
        // answering. The one toggle still in flight on the box exactly as the
        // loop keyed it is exempt: it may not have landed yet (above).
        if let QuestionForm::Multi { options, .. } = &dialog.form {
            let mine = self
                .question
                .toggled
                .get(&question_key(dialog, &turn.screen.rows));
            let made = |n: u8| mine.is_some_and(|m| m.contains(&n));
            if let Some(o) = options
                .iter()
                .find(|o| o.checked == Some(true) && !made(o.n))
            {
                return Ok(Some(Hold::HandOver(begun(&format!(
                    "option {} is checked, and the supervisor did not check it",
                    o.n
                )))));
            }
            if let Some(o) = options
                .iter()
                .find(|o| made(o.n) && o.checked != Some(true) && toggling != Some(o.n))
            {
                return Ok(Some(Hold::HandOver(begun(&format!(
                    "option {}, which the supervisor checked, reads unchecked",
                    o.n
                )))));
            }
        }
        // 3. A re-asked question is answered again (O5), after its pause.
        if let Some(q) = answering.filter(|_| !retrying) {
            let answered = approved
                .iter()
                .filter(|a| a.question.as_deref() == Some(q))
                .count();
            let waited = self
                .question
                .reasked
                .as_ref()
                .is_some_and(|(k, n)| k == q && *n == answered);
            if answered >= REASKED_AFTER && !waited {
                self.question.reasked = Some((q.to_string(), answered));
                let pause = reask_pause(answered);
                review.note(&format!(
                    "WAITING seq={seq} rule={rule_id} the same question came back after \
                     {answered} answers since the last review point (the model is re-asking \
                     it): answered again in {} ms",
                    pause.as_millis()
                ));
                self.wait(&["seq", &seq.to_string()], pause)?;
                return Ok(Some(Hold::Again));
            }
        }
        Ok(None)
    }

    /// An answer that landed at `at` on the box read at `seq`, ledgered and
    /// said: an approval `approved` (its `reason` the `unproven:` of a press
    /// no safe rule proved) and its `APPROVED` line to `review`; a decline
    /// `declined` (its `reason` the text typed) and a journal-only `DECLINED
    /// seq=<at> rule=<id> <subject> => <text>`, never a line a manager waits
    /// on, a badge or mail.
    fn answered(
        &mut self,
        act: &Act,
        rule_id: &str,
        shown: &str,
        reason: &str,
        (at, seq): (u64, u64),
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        match act {
            Act::Approve { .. } => {
                self.ledger_row(rule_id, approvals::Outcome::Approved, shown, reason, seq);
                review.approved(at, shown)?;
            }
            Act::Decline { text, .. } => {
                self.ledger_row(rule_id, approvals::Outcome::Declined, shown, reason, seq);
                review.note(&format!(
                    "DECLINED seq={at} rule={rule_id} {} => {text}",
                    clip(shown)
                ));
            }
        }
        Ok(())
    }

    /// The pause before the box `key` (`press.rs`'s `decline_box`) is
    /// declined again, when it came back [`REASKED_AFTER`] or more times since
    /// the last review point and that pause ([`reask_pause`]) has not been
    /// waited yet: the worker has not acted on its reason yet, and nobody
    /// else is there to answer it.
    fn decline_again(&mut self, key: &str, approved: &[Approved]) -> Option<Duration> {
        let declines = approved
            .iter()
            .filter(|a| a.declined.as_deref() == Some(key))
            .count();
        let waited = self
            .decline
            .again
            .as_ref()
            .is_some_and(|(k, n)| k == key && *n == declines);
        if declines < REASKED_AFTER || waited {
            return None;
        }
        self.decline.again = Some((key.to_string(), declines));
        Some(reask_pause(declines))
    }

    /// The journal's `WAITING seq=<n> rule=<the decision's rule> <why>`, once
    /// per box however many waits it takes.
    fn note_question_wait(
        &mut self,
        review: &mut dyn Review,
        seq: u64,
        rule_id: &str,
        identity: &str,
        why: &str,
    ) {
        if self.question.waiting_said.as_deref() != Some(identity) {
            self.question.waiting_said = Some(identity.to_string());
            review.note(&format!("WAITING seq={seq} rule={rule_id} {why}"));
        }
    }

    /// Whether a box pressed at `at` left ([`Self::moved_past`]), a move that
    /// leaves the same box ([`box_identity`] equal to `identity`) being no
    /// move: the wait goes on past it for at most one [`WAIT_STEP`] from the
    /// press, and then the box as read is [`Past::Still`] ("the box did not
    /// change after the press").
    fn box_left(&mut self, at: u64, identity: &str, deadline: Instant) -> Result<Past, Fail> {
        let until = self.fx.mono() + WAIT_STEP;
        let mut since = at;
        // The first wait is one whole step, as every wait is; a later one
        // what is left of it.
        let mut by = deadline;
        loop {
            match self.moved_past(since, by, WAIT_STEP)? {
                Past::Moved => {}
                Past::Still(None) if self.fx.mono() < deadline => {
                    // The step from the press ran out, the budget did not.
                    return Ok(Past::Still(Some(self.screen()?)));
                }
                still => return Ok(still),
            }
            let now = self.screen()?;
            if box_identity(&now.rows).as_deref() != Some(identity) {
                // The box left: the next one mounts afresh, and waits out its
                // own settle ([`Self::box_young`]).
                self.box_gone();
                return Ok(Past::Moved);
            }
            if self.fx.mono() >= until {
                return Ok(Past::Still(Some(now)));
            }
            since = now.seq;
            by = deadline.min(until);
        }
    }
}

/// The journal-only line of a press no safe rule proved, after its
/// `APPROVED`: `UNPROVEN seq=<n> rule=<id> unproven: <why>` (the journal's
/// `unproven` record, the rule as its phase). Printed nowhere: `watch`'s
/// lines are the same with and without it.
fn note_unproven(review: &mut dyn Review, seq: u64, rule_id: &str, unproven: Option<&str>) {
    if let Some(u) = unproven {
        review.note(&format!("UNPROVEN seq={seq} rule={rule_id} {u}"));
        // The warning an unproven rm breaker is approved with under `approve
        // = "safe"`: said, not only journaled, since it is the one line that
        // stands where the person's box would.
        if rule_id == RULE_RM_BREAKER_WARN {
            let _ = review.say(&format!("WARN seq={seq} rule={rule_id} {u}"));
        }
    }
}

/// Muse's focus dialogs are fenced on their own person and question. An
/// exhaustive match, so a new agent variant is named here by the compiler
/// instead of silently taking the `false` branch (`grep_guard`'s agent-gate).
fn is_muse(program: &Program) -> bool {
    match program {
        Program::Muse => true,
        Program::Claude | Program::Codex | Program::Agy | Program::Gemini | Program::Generic => {
            false
        }
    }
}

#[cfg(test)]
mod act_tests {
    use super::*;
    use crate::harness::acts::{Act, TaskFace};
    use crate::supervise::approvals::Outcome;
    use crate::supervise::policy::question::RULE_ANSWER_RECOMMENDED;
    use crate::supervise::policy::turn_end::{
        RULE_ANSWER, RULE_COMPACT, RULE_CONTINUE, RULE_LAND, RULE_LAND_NUDGE, RULE_LOGIN,
    };
    use std::sync::Mutex;

    /// A server that answers nothing: [`Session::ledger_row`] asks it
    /// nothing.
    struct Silent;

    impl Ctl for Silent {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            Err(format!("no server for {args:?}"))
        }
    }

    /// A host that records every act it is told, and wants nothing else.
    #[derive(Debug, Default)]
    struct Recorder {
        acts: Mutex<Vec<Act>>,
    }

    impl IdleHost for Recorder {
        fn wants(&self) -> bool {
            false
        }
        fn at_idle(&self) -> Option<HostStep> {
            None
        }
        fn owns_turn_end(&self) -> bool {
            false
        }
        fn act_taken(&self, act: Act) {
            self.acts.lock().unwrap().push(act);
        }
        fn task_face(&self, face: TaskFace) {
            let _ = face;
        }
    }

    /// Every row a loop writes in a session that approves a box, answers a
    /// three-key question dialog, answers in prose, keeps the work moving,
    /// compacts a full context and carries on, lands and nudges, declines a
    /// box, logs in, and writes the rows that are no act.
    const ROWS: [(&str, Outcome, &str); 15] = [
        ("allow-once@v1", Outcome::Approved, "cargo test"),
        (RULE_ANSWER_RECOMMENDED, Outcome::Approved, ""),
        (RULE_ANSWER_RECOMMENDED, Outcome::Approved, ""),
        (RULE_ANSWER_RECOMMENDED, Outcome::Approved, ""),
        (RULE_ANSWER, Outcome::Typed, "Decide for yourself."),
        (RULE_CONTINUE, Outcome::Typed, "keep going"),
        (RULE_COMPACT, Outcome::Typed, "/compact"),
        (RULE_COMPACT, Outcome::Typed, "Carry on where you left off."),
        (RULE_LAND, Outcome::Typed, "[aterm harness] land"),
        (
            RULE_LAND_NUDGE,
            Outcome::Typed,
            "[aterm harness] still landing",
        ),
        ("decline@v1", Outcome::Declined, "rm -rf /"),
        (RULE_LOGIN, Outcome::Typed, "/login"),
        ("read-only@classify-v3", Outcome::Skipped, "ls"),
        ("-", Outcome::Escalated, ""),
        ("wall-wait@v1", Outcome::Waiting, ""),
    ];

    /// Write [`ROWS`] through a loop's one ledger writer, `host` its host, to
    /// a ledger under `dir`: the rows read back off the file, each with what
    /// [`crate::harness::acts::sort`] makes of it.
    fn write_rows(dir: &std::path::Path, host: Option<Arc<dyn IdleHost>>) -> Vec<Option<Act>> {
        let path = dir.join("s-1.jsonl");
        let mut ctl = Silent;
        let mut s = Session::new(&mut ctl, Some("s-1".to_string()));
        s.set_approval_ledger(Some(path.clone()));
        s.ledger = approvals::Ledger::open(Some(&path), Some("s-1"), &mut std::io::sink());
        s.stall_host = host;
        for (i, (rule, outcome, command)) in ROWS.into_iter().enumerate() {
            s.ledger_row(rule, outcome, command, "", i as u64);
        }
        let body = std::fs::read_to_string(&path).expect("the ledger");
        body.lines()
            .map(|line| {
                let v: aterm_json::Value = aterm_json::from_str(line).expect("a row");
                let text = |k: &str| {
                    v.get(k)
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                let outcome = Outcome::from_word(&text("decision")).expect("its decision");
                crate::harness::acts::sort(&text("rule_id"), outcome, &text("command")).act
            })
            .collect()
    }

    /// ONE TELL PER ACT ROW (design §9.2.5): a recording host is told
    /// exactly one `act_taken` per ledger row whose sort names an act, in the
    /// ledger's order — three for the dialog's three keys, one for the
    /// `/compact`'s continuation — and none for the `/compact`, the login,
    /// or the `skipped`, `escalated` and `waiting` rows.
    ///
    /// NEGATIVE CONTROL: with no host (a CLI `watch` or `supervise`) every
    /// row is still written, and nothing is told.
    #[test]
    fn the_ledgers_writer_tells_its_host_one_act_per_act_row() {
        let dir = crate::supervise::ScratchDir::new("acts", "told");
        let host = Arc::new(Recorder::default());
        let read = write_rows(&dir, Some(Arc::clone(&host) as Arc<dyn IdleHost>));
        assert_eq!(read.len(), ROWS.len(), "every row written");
        let told = host.acts.lock().unwrap().clone();
        let acts: Vec<Act> = read.into_iter().flatten().collect();
        assert_eq!(told, acts);
        assert_eq!(
            told,
            [
                Act::Approved,
                Act::Answered,
                Act::Answered,
                Act::Answered,
                Act::Answered,
                Act::Continued,
                Act::Resumed,
                Act::Landing,
                Act::Nudged,
                Act::Declined,
            ]
        );
        // NEGATIVE CONTROL: no host.
        let bare = crate::supervise::ScratchDir::new("acts", "bare");
        assert_eq!(write_rows(&bare, None).len(), ROWS.len());
        assert_eq!(host.acts.lock().unwrap().len(), told.len());
    }
}
