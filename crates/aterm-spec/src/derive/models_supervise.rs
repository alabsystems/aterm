// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's own bounded machines (aterm-agent `supervise`, lane B2):
//! the session claim it acts under, the focus-then-Enter choice it makes on
//! an unnumbered dialog, the turn-end policy, the keys it sends to answer
//! Claude Code's question dialog, and the keystrokes of a decline.

use super::Model;

/// The SUPERVISOR CLAIM (aterm-agent `supervise/claim.rs`): a loop presses
/// only while its last word from the server was that the claim is its own
/// (`view` 1), and that word is renewed before any request once `Renew`
/// ticks have passed — so a lease that lapsed (`Ttl` ticks without a
/// request) and was taken by another supervisor is found out by the
/// renewal that precedes the very next press, never after it.
///
/// `held` is the server's claim (0 none, 1 this loop's, 2 another's);
/// `view` the loop's last reading of it (1 its own, 2 another's, 3 PENDING:
/// its last claim request was not served — asked again before the next
/// request, pressing nothing until answered); `age` the ticks since the
/// loop last asked. `Buggy = 1` presses without the fresh
/// renewal — the stale-view press — and is caught: a lapse, another's claim,
/// then a press under it. Tier-1 (aterm-agent `run_engine_tests.rs`) drives
/// the real `Session` over the scripted server and projects its claim
/// state onto these variables. A host with no claim to take (`CLAIM
/// unavailable`: a build before lane C) is outside this model: the loop
/// acts there as it did before the claim existed.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_claim_model() -> Model {
    crate::ty_model! {
        SupervisorClaim {
            const Buggy = 0;
            const Renew = 1;
            const Ttl = 3;
            var held = 0;
            var view = 0;
            var age = 0;
            var pressed_under_other = 0;

            // Time passes with no request from the loop.
            action Tick when (age <= Ttl - 1) {
                age = age + 1;
            }
            // The server lets an unrenewed lease go.
            action Lapse when (held == 1 && age == Ttl) {
                held = 0;
            }
            action OtherClaims when (held == 0) {
                held = 2;
            }
            action OtherReleases when (held == 2) {
                held = 0;
            }
            // Any request of the loop's, preceded by the renewal once it is
            // due: the server grants a free claim and refuses a held one.
            action Renewal when (Renew <= age || view == 3) {
                view = if held == 2 { 2 } else { 1 };
                held = if held == 2 { 2 } else { 1 };
                age = 0;
            }
            // A claim request the server did not serve (an outage): the
            // loop cannot tell whose the session is.
            action Unserved when (Renew <= age || view == 3) {
                view = 3;
                age = 0;
            }
            action Press when (view == 1 && (age <= Renew - 1 || Buggy == 1)) {
                pressed_under_other = if held == 2 { 1 } else { pressed_under_other };
            }

            invariant NoPressUnderAnothersClaim: pressed_under_other == 0;
        }
    }
}

/// The CHOICE ON AN UNNUMBERED DIALOG (aterm-agent `supervise/press.rs`,
/// `press_focused`; the folder-trust dialog): the focus is moved, the screen
/// read again, and Enter is pressed only when that read shows the focus on
/// the chosen option AND the screen is still the one that read saw (the
/// generation fence) — so an Enter never lands on `No, exit`.
///
/// `focus` is where the dialog's cursor is (0 `No, exit`, 1 the chosen
/// option); `confirmed` that the last read showed it on the chosen one;
/// `fresh` that the screen has not moved since that read. A move may land
/// or be lost; the screen may move (and the focus with it) at any time. A
/// move the dialog did not take — a fresh read shows the focus where it was
/// — is made AGAIN, at most `Tries` times (a dialog drawn before its input
/// is live drops the key: measured live 2026-09-24; the loop's
/// `MAX_CHANGED_PRESSES` less the first move). `Buggy = 1` presses Enter on
/// the move alone, unread, and is caught.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_focus_choice_model() -> Model {
    crate::ty_model! {
        SupervisorFocusChoice {
            const Buggy = 0;
            const Tries = 4;
            var focus = 0;
            var moved = 0;
            var confirmed = 0;
            var fresh = 0;
            var entered = 0;
            var entered_on_other = 0;
            var tries = 0;

            action MoveLands when (
                entered == 0 &&
                (moved == 0 || (fresh == 1 && confirmed == 0 && focus == 0 && tries <= Tries - 1))
            ) {
                tries = if moved == 1 { tries + 1 } else { tries };
                moved = 1;
                focus = 1;
                fresh = 0;
            }
            action MoveLost when (
                entered == 0 &&
                (moved == 0 || (fresh == 1 && confirmed == 0 && focus == 0 && tries <= Tries - 1))
            ) {
                tries = if moved == 1 { tries + 1 } else { tries };
                moved = 1;
                fresh = 0;
            }
            action Reread when (moved == 1 && entered == 0) {
                confirmed = focus;
                fresh = 1;
            }
            action ScreenMoves when (entered == 0) {
                focus = 0;
                fresh = 0;
            }
            action Enter when (
                entered == 0 && moved == 1 &&
                ((confirmed == 1 && fresh == 1) || Buggy == 1)
            ) {
                entered = 1;
                entered_on_other = if focus == 1 { 0 } else { 1 };
            }

            invariant NeverEnterOffTheChosenOption: entered_on_other == 0;
        }
    }
}

/// THE TURN-END POLICY (aterm-agent `supervise/policy/turn_end.rs`,
/// `decide_turn_end`), FULLY AUTOMATIC (owner, 2026-09-24): what the
/// supervisor may type at a point where a worker's turn has ended. A
/// CONTINUATION goes only with no box up, no wall, no person at the
/// keyboard (their keystroke's grace running), nothing typed in the
/// composer and — after a short turn — once its BACK-OFF is over; a wall's
/// RETRY goes once the wall's wait is over, for ever (no tries to spend). A
/// DRAFT left in the composer is never typed over: once its person's grace
/// has passed it is SUBMITTED where the policy would act (a continuation's
/// place, or a retry's). Nothing is ESCALATED at a point the policy can
/// answer: only past a cap the owner wrote (`Budget` acts in the window;
/// `0`, the default, is none).
///
/// `box_up` a box on the screen; `wall` 0 none, 1 up with its wait running,
/// 2 up with its wait over; `person` 1 while a person's keystroke is inside
/// the grace; `used` the typed acts in the cap's window (`HourPasses`
/// empties it); `short` the short turns in a row (saturating at `Streak`:
/// a longer streak backs off as long, the real ladder's cap); `backoff` 1
/// while the back-off after a short turn runs (`BackoffDue` ends it, and the
/// real Tier-1 moves its clock by exactly the real ladder's wait for that
/// streak: 2, 4, 8 … 60 min); `pending` 0 at a point, 1 a continuation's
/// turn running, 2 a retry's, 3 a continuation's reply ended with NO busy
/// read, `waited` the ticks its point has shown so, up to `Take` — the
/// policy's `take_within`, where the point is judged as the short yield it
/// is (`DeadlineUnseen`) — and so is an act whose `❯` row the worker never
/// took (the Tier-1 walks both). `draft` 1 while text stands in the composer
/// (typing it is a person's keystroke: its grace runs). `Buggy = 1` ignores
/// the box, the person, the draft and the back-off, escalates "worker
/// reports done" after two short turns (the rule until 2026-09-24), and
/// keeps awaiting an unseen point past its deadline (the silent latch lane
/// B2's review found) — each caught. Tier-1
/// (aterm-agent `tests/supervise_conformance_turn_end.rs`) drives the real
/// decider along every reachable state of this model, with no cap and with
/// one, and checks that it types exactly where the model allows it, waits
/// where a wall, a person or a back-off holds it, and is never silent at a
/// free point.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_turn_end_model() -> Model {
    crate::ty_model! {
        SupervisorTurnEnd {
            const Buggy = 0;
            const Budget = 0;
            const Take = 2;
            const Streak = 3;
            var box_up = 0;
            var wall = 0;
            var person = 0;
            var used = 0;
            var short = 0;
            var backoff = 0;
            var pending = 0;
            var waited = 0;
            var draft = 0;
            var typed_blocked = 0;
            var typed_over = 0;
            var typed_early = 0;
            var escalated = 0;
            var latched = 0;

            action BoxAppears when (box_up == 0 && pending == 0) {
                box_up = 1;
            }
            action BoxLeaves when (box_up == 1) {
                box_up = 0;
            }
            // (Each clock runs alone — a wall's wait, a back-off, the cap's
            // window — so the real side can move its clock by exactly the
            // one that runs out.)
            action WallAppears when (
                wall == 0 && box_up == 0 && pending == 0 && backoff == 0
            ) {
                wall = 1;
            }
            action WallDue when (wall == 1) {
                wall = 2;
            }
            action PersonTypes when (person == 0 && pending <= 2) {
                person = 1;
            }
            // Text left in the composer: typed by a person, whose grace runs.
            action DraftLeft when (draft == 0 && pending == 0 && box_up == 0) {
                draft = 1;
                person = 1;
            }
            action GraceEnds when (person == 1) {
                person = 0;
            }
            // (Not while an unseen point's deadline runs: that clock is
            // the deadline's, `TimePassesUnseen`.)
            action HourPasses when (used > 0 && wall == 0 && backoff == 0 && pending <= 2) {
                used = 0;
            }
            action BackoffDue when (backoff == 1 && pending == 0) {
                backoff = 0;
            }
            // Someone else's turn of real work (a draft among it sent): the
            // streak ends.
            action HumanWork when (pending == 0 && box_up == 0 && wall == 0) {
                short = 0;
                backoff = 0;
                draft = 0;
            }
            action Continue when (
                pending == 0 && wall == 0 &&
                ((box_up == 0 && person == 0 && draft == 0 && backoff == 0) || Buggy == 1) &&
                (Budget == 0 || used <= Budget - 1)
            ) {
                typed_blocked = if box_up == 1 || person == 1 { 1 } else { typed_blocked };
                typed_over = if draft == 1 { 1 } else { typed_over };
                typed_early = if backoff == 1 { 1 } else { typed_early };
                used = if Budget == 0 { used } else { used + 1 };
                pending = 1;
            }
            action WorkedShort when (pending == 1) {
                pending = 0;
                short = if short <= Streak - 1 { short + 1 } else { short };
                backoff = 1;
            }
            action WorkedLong when (pending == 1) {
                pending = 0;
                short = 0;
                backoff = 0;
            }
            // The reply ends and no read saw the worker busy.
            action ReplyUnseen when (pending == 1) {
                pending = 3;
                waited = 0;
            }
            action TimePassesUnseen when (pending == 3 && waited <= Take - 1) {
                waited = waited + 1;
            }
            // The deadline: the point is the act's, with no work seen.
            action DeadlineUnseen when (pending == 3 && waited == Take) {
                latched = if Buggy == 1 { 1 } else { latched };
                pending = if Buggy == 1 { 3 } else { 0 };
                short = if Buggy == 1 || short == Streak { short } else { short + 1 };
                backoff = if Buggy == 1 { backoff } else { 1 };
                waited = 0;
            }
            action Retry when (
                pending == 0 && wall == 2 &&
                ((box_up == 0 && person == 0 && draft == 0) || Buggy == 1) &&
                (Budget == 0 || used <= Budget - 1)
            ) {
                typed_blocked = if box_up == 1 || person == 1 { 1 } else { typed_blocked };
                typed_over = if draft == 1 { 1 } else { typed_over };
                used = if Budget == 0 { used } else { used + 1 };
                pending = 2;
            }
            // The draft sent as it stands, in the act's place: a
            // continuation's turn, or at a wall whose wait is over its
            // retry's.
            action SubmitDraft when (
                pending == 0 && draft == 1 && (wall == 0 || wall == 2) &&
                box_up == 0 && person == 0 && backoff == 0 &&
                (Budget == 0 || used <= Budget - 1)
            ) {
                draft = 0;
                used = if Budget == 0 { used } else { used + 1 };
                pending = if wall == 2 { 2 } else { 1 };
            }
            action RetryTaken when (pending == 2) {
                pending = 0;
                wall = 0;
            }
            action RetryHitsTheWall when (pending == 2) {
                pending = 0;
                wall = 1;
            }
            // A person is told only past the owner's cap — or, `Buggy`, of
            // a worker that "reports done".
            action Escalate when (
                pending == 0 && box_up == 0 && person == 0 && (
                    (Budget > 0 && used == Budget && backoff == 0 && (wall == 0 || wall == 2)) ||
                    (Buggy == 1 && short > 1)
                )
            ) {
                escalated = if Budget == 0 { 1 } else { escalated };
            }

            invariant NeverTypeUnderABoxOrAPerson: typed_blocked == 0;
            invariant NeverTypeOverADraft: typed_over == 0;
            invariant NeverTypeBeforeItsBackoff: typed_early == 0;
            invariant NeverEscalateUnderFullPower: escalated == 0;
            invariant WithinTheCap: Budget == 0 || used <= Budget;
            invariant NoSilentLatch: latched == 0;
        }
    }
}

/// THE QUESTION ANSWER (aterm-agent `supervise/policy/question.rs`, the
/// rule `answer-recommended@v1`; its keys sent by `supervise/press.rs`'s
/// `press_question` under `supervise/approval_loop.rs`'s waits): the keys
/// the supervisor sends to answer Claude Code's question dialog — `↑` or
/// `↓` to move the focus onto the option it chose, Enter on it, never a digit
/// (the critique of 2026-09-25, R4) — land only on the dialog state they
/// were decided on, although the PTY carries keys Claude Code has not read
/// yet, a person's as well as the supervisor's own (R3c).
///
/// **The screen generation fence alone does not prove that** (R3): it
/// proves the SCREEN did not change between the read a key was decided on
/// and its write, and a key written to the PTY that Claude Code has not
/// read yet changes nothing on the screen. So every key is split here into
/// its WRITE (`PersonKey`, `HarnessMove`, `HarnessEnter`: appended to the
/// PTY's queue `q1`, `q2`, oldest first — 0 none, 1 a person's key, 2 the
/// supervisor's `↑` or `↓` (the model abstracts the direction: a move is
/// one row toward the chosen one), `3 + t` its Enter decided on tab `t`)
/// and its CONSUME
/// by Claude Code (the head's effect on the dialog as it is THEN: `MoveLands`,
/// `MoveShort`, `EnterTaken`, the person's `PersonMovesOff` … `PersonAnswers`,
/// `KeyRefused` for a key the dialog's typeahead window drops, O4, and
/// `KeyIntoTheComposer` for one read after the dialog closed). What keeps a
/// supervisor key off a state it was not decided on is two guards the loop
/// adds to the decision (and `Buggy = 1` drops both — the fence alone, as
/// first specified):
///
/// * **A person's quiet** (R1, `quiet`): the loop keys nothing unless the
///   server's per-session person stamp (`"human_ms"`) is at least
///   `[harness] human_grace_s` old, or no person ever keyed the session. A
///   person's write zeroes it; it returns (`QuietReturns`) only once their
///   keys are consumed — so a person's key is never ahead of the
///   supervisor's in the queue.
/// * **The progression rule** (R3a, `mine`): after its key the loop keys
///   nothing while a read shows the box as it was keyed (the same tab,
///   focus, focus row and free-text row: `Read` keeps `mine` only then). On
///   such a read it waits for the screen to hold still (`StillScreen`) and
///   only then sends the key again (`retried` counts the tries on that
///   box). The real loop, with nobody to hand the box to, tries for as long
///   as the dialog stands, each try after the press back-off (the
///   philosophy review of 2026-09-25, P3); `supervise`'s one look tries
///   once and hands the box to its manager. `MaxRetry` bounds the checked
///   space, not the loop: every try is guarded alike, so the proof at each
///   bound is the proof of the next try — `aterm-spec/tests/
///   derived_supervise.rs` proves it at 1 (the one look), 2 (committed) and
///   3. So its own earlier key is never ahead of its next one either.
///
/// With both, every supervisor key is written on an EMPTY queue, is the
/// next key Claude Code reads, and meets the dialog as the read showed it —
/// which is the whole of the four invariants: no Enter in the free-text
/// field or on any row but the chosen one (`NoDigitInTheField`, named for
/// the digit the first design sent: `focus` 1 is every row but the chosen
/// one — the free-text row, the chat row, another option), none on a tab
/// after the one it was decided on (`NeverAnswersANextTabWithAStaleKey`),
/// none on the review's cancel (`NeverCancelsTheReview`: the tool call is
/// rejected and the model told to STOP, S7-03), and none read after the
/// dialog closed (`NoKeyIntoTheComposer`: `↑` recalls history into it).
///
/// `tab` is the dialog's tab (`0..Tabs-1` a question, `Tabs` the review,
/// `Tabs + 1` closed; with `Review = 0` — a lone single-select question,
/// which draws no Submit tab — the last question's answer closes it);
/// `focus` 0 the chosen row (the recommended option; the review's `1.
/// Submit answers`) and 1 any other; `row` the focus row's parity in the
/// focus order, so a move shows on the next read even when it lands on
/// another row that is not the chosen one; `typed` a person's text in the
/// free-text row. A question tab may OPEN with the focus off the chosen row
/// (`OpensOffChoice`, before its first read: the vendor puts it on option
/// 1, and the recommendation may be option 2), and a multi-select's toggle
/// is modelled as a tab of its own — an Enter on the chosen row whose
/// consume moves on to the next choice (the next recommended option, then
/// the button), with the focus left where it was. `fresh` is the loop's last read still the
/// screen (the fence: every visible change clears it, and a write spends
/// it), `seen_*` what that read showed. The decision itself (the pure
/// `answer_question`, Tier-1 bound in its `question_tests.rs`): Enter on the
/// chosen row, a move toward it from any other, nothing (escalated) over a
/// person's text or once the dialog closed.
///
/// **Two ASSUMPTIONS, not mechanisms** — each a constant at 1 here, and
/// each refuted at 0 by `aterm-spec/tests/derived_supervise.rs`, so the
/// model says exactly what it rests on:
///
/// * `QuietAtWrite`: the person's quiet holds AT the write. The loop reads
///   the stamp on the read the key is decided on (and, for a tab's first
///   key, written after O4's wait of up to 3 s, on a `status` read just
///   before it), so a person's key written in that last round trip and not
///   yet read by Claude Code is the residual — the deferred `key if-input=`
///   server fence (R3b) would close it. At 0 (the quiet taken from the
///   read, `seen_quiet`) the invariants are refuted.
/// * `TakenBeforeStill`: a key written before the screen held still for 2 s
///   was read by Claude Code — taken or refused — by then; the retry rests
///   on it. At 0 the retry answers the next tab with a stale key.
///
/// Out of the model: a person's keys are consumed within the quiet window
/// (`QuietReturns` waits for it), the queue holds two keys (with the guards
/// the supervisor's key is always the oldest, so nothing behind it
/// matters), and a multi-select toggle is an Enter that does not advance —
/// written under the same guards, so met in the state it was decided on as
/// the advancing Enter here is. Tier-1 (aterm-agent
/// `supervise/run_engine_tests.rs`) drives the real `Session` over the
/// scripted server and projects each read, key and wait onto this model:
/// every key the real loop sends must be enabled here, at the state the
/// environment actions that explain its reads lead to.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_question_answer_model() -> Model {
    crate::ty_model! {
        SupervisorQuestionAnswer {
            const Buggy = 0;
            const Tabs = 2;
            const QuietAtWrite = 1;
            const TakenBeforeStill = 1;
            const Review = 1;
            const MaxRetry = 2;
            var tab = 0;
            var focus = 0;
            var row = 0;
            var typed = 0;
            var q1 = 0;
            var q2 = 0;
            var quiet = 1;
            var fresh = 0;
            var seen_tab = 0;
            var seen_focus = 0;
            var seen_row = 0;
            var seen_typed = 0;
            var seen_quiet = 1;
            var mine = 0;
            var stilled = 0;
            var retried = 0;
            var in_field = 0;
            var stale = 0;
            var cancelled = 0;
            var composer = 0;
            var opened = 1;

            // ---- a person writes a key (any: its effect is its consume's) ----
            action PersonKey when (tab <= Tabs && q2 == 0) {
                q1 = if q1 == 0 { 1 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 1 };
                quiet = 0;
            }
            // Their stamp ages past the quiet window, their keys consumed.
            action QuietReturns when (
                quiet == 0 && (q1 == 0 || 2 <= q1) && (q2 == 0 || 2 <= q2)
            ) {
                quiet = 1;
            }

            // ---- Claude Code reads the oldest key in the queue ----
            action PersonMovesOff when (q1 == 1 && tab <= Tabs) {
                focus = 1;
                row = 1 - row;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonMovesOnto when (q1 == 1 && tab <= Tabs) {
                focus = 0;
                row = 1 - row;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonTypes when (q1 == 1 && tab <= Tabs - 1 && focus == 1) {
                typed = 1;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            // Their Enter, digit or Tab: the tab answered or skipped, the
            // review submitted or cancelled — theirs to do.
            action PersonAnswers when (q1 == 1 && tab <= Tabs) {
                tab = if tab == Tabs - 1 && Review == 0 { Tabs + 1 } else { tab + 1 };
                focus = 0;
                row = 0;
                typed = 0;
                q1 = q2;
                q2 = 0;
                fresh = 0;
                opened = 1;
            }
            action PersonKeyIgnored when (q1 == 1) {
                q1 = q2;
                q2 = 0;
            }
            // A move (`↑` or `↓`) onto the next row: the chosen one from
            // elsewhere, off it from it.
            action MoveLands when (q1 == 2 && tab <= Tabs) {
                focus = 1 - focus;
                row = 1 - row;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            // A move one row nearer the chosen one, not on it yet.
            action MoveShort when (q1 == 2 && tab <= Tabs && focus == 1) {
                row = 1 - row;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            // Enter answers the focused row: the tab moves on, the review
            // submits (or, on its cancel, rejects the tool call).
            action EnterTaken when (3 <= q1 && tab <= Tabs) {
                stale = if q1 == 3 + tab { stale } else { 1 };
                in_field = if tab <= Tabs - 1 && focus == 1 { 1 } else { in_field };
                cancelled = if tab == Tabs && focus == 1 { 1 } else { cancelled };
                tab = if tab == Tabs - 1 && Review == 0 { Tabs + 1 } else { tab + 1 };
                focus = 0;
                row = 0;
                typed = 0;
                q1 = q2;
                q2 = 0;
                fresh = 0;
                opened = 1;
            }
            // A question tab drawn with its focus off the chosen row (the
            // vendor's option 1, the recommendation option 2; a toggled
            // option, the next choice another row) — before any read of it.
            action OpensOffChoice when (opened == 1 && tab <= Tabs - 1) {
                focus = 1;
                row = 1 - row;
                fresh = 0;
            }
            // Dropped inside the dialog's typeahead window (O4): no effect.
            action KeyRefused when (2 <= q1) {
                q1 = q2;
                q2 = 0;
            }
            action KeyIntoTheComposer when (2 <= q1 && Tabs + 1 <= tab) {
                composer = 1;
                q1 = q2;
                q2 = 0;
            }

            // ---- the supervisor ----
            // A read of the dialog; the loop's last key is still "its"
            // (`mine`) only while the read shows the box as it was keyed.
            action Read when (tab <= Tabs) {
                fresh = 1;
                opened = 0;
                seen_tab = tab;
                seen_focus = focus;
                seen_row = row;
                seen_typed = typed;
                seen_quiet = quiet;
                mine = if mine == 1 && tab == seen_tab && focus == seen_focus
                    && row == seen_row && typed == seen_typed { 1 } else { 0 };
                stilled = if mine == 1 && tab == seen_tab && focus == seen_focus
                    && row == seen_row && typed == seen_typed { stilled } else { 0 };
                retried = if mine == 1 && tab == seen_tab && focus == seen_focus
                    && row == seen_row && typed == seen_typed { retried } else { 0 };
            }
            // The read showed the box as keyed, and the screen then held
            // still 2 s (`await idle 2000`): by then its key was read.
            action StillScreen when (
                mine == 1 && stilled == 0 && fresh == 1 &&
                (TakenBeforeStill == 0 || (q1 <= 1 && q2 <= 1))
            ) {
                stilled = 1;
            }
            action HarnessEnter when (
                fresh == 1 && q2 == 0 && seen_tab <= Tabs && seen_typed == 0 &&
                seen_focus == 0 &&
                (Buggy == 1 || (
                    ((QuietAtWrite == 1 && quiet == 1) || (QuietAtWrite == 0 && seen_quiet == 1)) &&
                    (mine == 0 || (stilled == 1 && retried <= MaxRetry - 1))
                ))
            ) {
                q1 = if q1 == 0 { 3 + seen_tab } else { q1 };
                q2 = if q1 == 0 { 0 } else { 3 + seen_tab };
                retried = if mine == 0 { 0 }
                    else { if retried <= MaxRetry - 1 { retried + 1 } else { retried } };
                stilled = 0;
                mine = 1;
                fresh = 0;
            }
            action HarnessMove when (
                fresh == 1 && q2 == 0 && seen_tab <= Tabs && seen_typed == 0 &&
                seen_focus == 1 &&
                (Buggy == 1 || (
                    ((QuietAtWrite == 1 && quiet == 1) || (QuietAtWrite == 0 && seen_quiet == 1)) &&
                    (mine == 0 || (stilled == 1 && retried <= MaxRetry - 1))
                ))
            ) {
                q1 = if q1 == 0 { 2 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 2 };
                retried = if mine == 0 { 0 }
                    else { if retried <= MaxRetry - 1 { retried + 1 } else { retried } };
                stilled = 0;
                mine = 1;
                fresh = 0;
            }

            invariant NoDigitInTheField: in_field == 0;
            invariant NeverAnswersANextTabWithAStaleKey: stale == 0;
            invariant NeverCancelsTheReview: cancelled == 0;
            invariant NoKeyIntoTheComposer: composer == 0;
        }
    }
}

/// THE DECLINE'S KEYSTROKES (aterm-agent `supervise/press.rs`,
/// `Session::press_decline`; the review of 2026-09-25, F1 and F3): a Bash
/// box's `No` amended with a reason the worker acts on — the focus moved
/// off `1. Yes` (`↓`), Tab on the shut `No` (its input opens), the reason
/// typed into the open, empty input, Enter on the input showing exactly the
/// reason — each decided on a fresh read of the box.
///
/// Each key is split into its WRITE (`HarnessDown`, `HarnessTab`,
/// `HarnessText`, `HarnessEnter`, and a person's `PersonKey`: appended to the
/// PTY's queue `q1`, `q2`, oldest first — 1 `↓`, 2 Tab, 3 the reason, 4
/// Enter, 5 a person's key) and its CONSUME by Claude Code, which meets the
/// box as it is THEN. That is where a key decided on a stale state does its
/// damage, and the screen generation fence cannot see it: a key written and
/// not yet read changes nothing on the screen. Tab TOGGLES the focused
/// option's input (measured on 2.1.282), so a second Tab behind a first not
/// yet drawn shuts the input again; and a closed Select reads typed text as
/// keys, so the reason's digits (`$1` in the quoted `rm -rf $S/$1`) choose
/// an option — `1` is `1. Yes`, on a box the harness could not read.
///
/// The guards (and `Buggy = 1` drops both, leaving the generation fence —
/// `fresh`: no visible change since the read — as the port first shipped
/// it):
///
/// * **One keystroke in flight** (`mine`): after a write the supervisor
///   writes nothing while a read shows the box as it was when it wrote (the
///   same focus, input and text: `Read` keeps `mine` only then). It never
///   writes that keystroke a second time — a keystroke not drawn after the
///   bounded reads is handed over, never re-sent — so its own earlier key is
///   never behind the queue's head when it writes.
/// * **A person's quiet** (`quiet`, R1 of the question answer): nothing is
///   written unless the server's person stamp is at least `[harness]
///   human_grace_s` old (a host that stamps nothing is handed the box). A person's write zeroes it; it returns only once their keys are
///   consumed.
///
/// With both, every supervisor key is written on an empty queue and meets
/// the box as the read showed it — the invariants: `1. Yes` is never chosen
/// by a supervisor key (`NeverChoosesTheAllow`) nor amended
/// (`NeverAmendsTheAllow`), the bare `No` (the shut refusal, or Enter on the
/// empty input: the worker stops to wait for a person) is never its answer
/// (`NeverTheBareNo`), it never submits a text that is not its reason
/// (`NeverSubmitsAnotherText`), and no key of its is read after the box left
/// (`NoKeyIntoTheComposer`). A person's own answers (`PersonAnswers`) are
/// theirs and flag nothing.
///
/// One ASSUMPTION, a constant at 1 here and refuted at 0 by
/// `aterm-spec/tests/derived_supervise.rs`: `QuietAtWrite`, the person's
/// quiet holds AT the write — the loop reads the stamp on the read the key
/// is fenced on (and on a `status` just before the first key), so a person's
/// key written in that last round trip is the residual (the deferred `key
/// if-input=` fence, R3b of the question answer, would close it).
///
/// `focus` 0 is `1. Yes` and 1 `2. No` (two options: a `↓` off the last
/// wraps); `open` the refusal's input; `text` 0 empty, 1 the reason, 2
/// anything else (a person's, or the reason twice); `gone` 0 up, 1 refused
/// with the reason, 2 answered otherwise. Tier-1 (aterm-agent
/// `supervise/run_engine_tests.rs`) drives the real press over the scripted
/// server and projects each read and write onto this model: every
/// keystroke the real press writes must be enabled here.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_decline_keys_model() -> Model {
    crate::ty_model! {
        SupervisorDeclineKeys {
            const Buggy = 0;
            const QuietAtWrite = 1;
            var focus = 0;
            var open = 0;
            var text = 0;
            var gone = 0;
            var approved = 0;
            var amended_yes = 0;
            var bare = 0;
            var foreign = 0;
            var composer = 0;
            var q1 = 0;
            var q2 = 0;
            var quiet = 1;
            var fresh = 0;
            var seen_focus = 0;
            var seen_open = 0;
            var seen_text = 0;
            var seen_quiet = 1;
            var mine = 0;

            // ---- a person writes a key (its effect is its consume's) ----
            action PersonKey when (gone == 0 && q2 == 0) {
                q1 = if q1 == 0 { 5 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 5 };
                quiet = 0;
            }
            // Their stamp ages past the quiet window, their keys consumed.
            action QuietReturns when (quiet == 0 && q1 <= 4 && q2 <= 4) {
                quiet = 1;
            }
            action PersonMoves when (q1 == 5 && gone == 0) {
                focus = 1 - focus;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonTabs when (q1 == 5 && gone == 0 && focus == 1) {
                open = 1 - open;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonTypes when (q1 == 5 && gone == 0 && open == 1 && focus == 1) {
                text = 2;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            // Their Enter, digit or Esc: the box answered — theirs to do.
            action PersonAnswers when (q1 == 5 && gone == 0) {
                gone = 2;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonKeyIgnored when (q1 == 5) {
                q1 = q2;
                q2 = 0;
            }

            // ---- Claude Code reads the supervisor's oldest key ----
            action DownTaken when (q1 == 1 && gone == 0) {
                focus = 1 - focus;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            // Tab toggles the FOCUSED option's input: the refusal's, or —
            // on `1. Yes` — the allow's own amend.
            action TabTaken when (q1 == 2 && gone == 0) {
                open = if focus == 1 { 1 - open } else { open };
                amended_yes = if focus == 0 { 1 } else { amended_yes };
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action TextIntoTheInput when (q1 == 3 && gone == 0 && open == 1 && focus == 1) {
                text = if text == 0 { 1 } else { 2 };
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            // A closed Select reads the text as keys: its `1` chooses the allow.
            action TextOnTheSelect when (q1 == 3 && gone == 0 && (open == 0 || focus == 0)) {
                approved = 1;
                gone = 2;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action EnterSubmitsTheReason when (
                q1 == 4 && gone == 0 && open == 1 && focus == 1 && text == 1
            ) {
                gone = 1;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action EnterSubmitsAnotherText when (
                q1 == 4 && gone == 0 && open == 1 && focus == 1 && 2 <= text
            ) {
                foreign = 1;
                gone = 2;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            // `allowEmptySubmitToCancel`: Enter on the empty input is the bare `No`.
            action EnterOnTheEmptyInput when (
                q1 == 4 && gone == 0 && open == 1 && focus == 1 && text == 0
            ) {
                bare = 1;
                gone = 2;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action EnterOnTheSelect when (q1 == 4 && gone == 0 && (open == 0 || focus == 0)) {
                approved = if focus == 0 { 1 } else { approved };
                bare = if focus == 1 { 1 } else { bare };
                gone = 2;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            // Dropped (a dialog's typeahead window): no effect.
            action KeyRefused when (1 <= q1 && q1 <= 4 && gone == 0) {
                q1 = q2;
                q2 = 0;
            }
            action KeyIntoTheComposer when (1 <= q1 && q1 <= 4 && 1 <= gone) {
                composer = 1;
                q1 = q2;
                q2 = 0;
            }

            // ---- the supervisor ----
            // A read of the box; its last keystroke is still in flight
            // (`mine`) only while the read shows the box as it was keyed.
            action Read when (gone == 0) {
                fresh = 1;
                seen_focus = focus;
                seen_open = open;
                seen_text = text;
                seen_quiet = quiet;
                mine = if mine == 1 && focus == seen_focus && open == seen_open
                    && text == seen_text { 1 } else { 0 };
            }
            action HarnessDown when (
                fresh == 1 && q2 == 0 && seen_focus == 0 && seen_open == 0 &&
                (Buggy == 1 || (mine == 0 &&
                    ((QuietAtWrite == 1 && quiet == 1) || (QuietAtWrite == 0 && seen_quiet == 1))))
            ) {
                q1 = if q1 == 0 { 1 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 1 };
                mine = 1;
                fresh = 0;
            }
            action HarnessTab when (
                fresh == 1 && q2 == 0 && seen_focus == 1 && seen_open == 0 &&
                (Buggy == 1 || (mine == 0 &&
                    ((QuietAtWrite == 1 && quiet == 1) || (QuietAtWrite == 0 && seen_quiet == 1))))
            ) {
                q1 = if q1 == 0 { 2 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 2 };
                mine = 1;
                fresh = 0;
            }
            action HarnessText when (
                fresh == 1 && q2 == 0 && seen_focus == 1 && seen_open == 1 && seen_text == 0 &&
                (Buggy == 1 || (mine == 0 &&
                    ((QuietAtWrite == 1 && quiet == 1) || (QuietAtWrite == 0 && seen_quiet == 1))))
            ) {
                q1 = if q1 == 0 { 3 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 3 };
                mine = 1;
                fresh = 0;
            }
            action HarnessEnter when (
                fresh == 1 && q2 == 0 && seen_focus == 1 && seen_open == 1 && seen_text == 1 &&
                (Buggy == 1 || (mine == 0 &&
                    ((QuietAtWrite == 1 && quiet == 1) || (QuietAtWrite == 0 && seen_quiet == 1))))
            ) {
                q1 = if q1 == 0 { 4 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 4 };
                mine = 1;
                fresh = 0;
            }

            invariant NeverChoosesTheAllow: approved == 0;
            invariant NeverAmendsTheAllow: amended_yes == 0;
            invariant NeverTheBareNo: bare == 0;
            invariant NeverSubmitsAnotherText: foreign == 0;
            invariant NoKeyIntoTheComposer: composer == 0;
        }
    }
}
