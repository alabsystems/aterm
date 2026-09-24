// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's own bounded machines (aterm-agent `supervise`, lane B2):
//! the session claim it acts under, and the focus-then-Enter choice it
//! makes on an unnumbered dialog.

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
/// `decide_turn_end`): what the supervisor may type at a point where a
/// worker's turn has ended. A CONTINUATION goes only with no box up, no
/// wall, the budget not spent and fewer than two continuations in a row
/// that each yielded short work ("worker reports done" escalates instead);
/// a wall's RETRY goes only once the wall's wait is over, within its tries
/// and the same budget. Nothing is typed while a box is up or a wall's wait
/// runs.
///
/// `box_up` a box on the screen; `wall` 0 none, 1 up with its wait running,
/// 2 up with its wait over; `tries` the retries this wall has had; `used`
/// the typed acts in the budget's window (`HourPasses` empties it); `short`
/// the continuations in a row that yielded short work; `pending` 0 at a
/// point, 1 a continuation's turn running, 2 a retry's, 3 a continuation's
/// reply ended with NO busy read (inside the `turn` verb's own settle, or
/// an Enter that did not take), `waited` the ticks its point has shown so,
/// up to `Take` — the policy's `take_within`, where the point is judged as
/// the short yield it is (`DeadlineUnseen`). `Budget` and `Tries` are
/// scaled down (the owner's 6 an hour and 3 retries). `Buggy = 1` ignores
/// the box — a continuation typed into an approval box — and keeps awaiting
/// an unseen point past its deadline — the silent latch lane B2's review
/// found — and both are caught. Tier-1 (aterm-agent
/// `tests/supervise_conformance_turn_end.rs`) drives the real decider along
/// every reachable state of this model and checks that it types exactly
/// where the model allows it and is never silent at a free point.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_turn_end_model() -> Model {
    crate::ty_model! {
        SupervisorTurnEnd {
            const Buggy = 0;
            const Budget = 2;
            const Tries = 3;
            const Take = 2;
            var box_up = 0;
            var wall = 0;
            var tries = 0;
            var used = 0;
            var short = 0;
            var pending = 0;
            var typed_blocked = 0;
            var typed_after_done = 0;
            var waited = 0;
            var latched = 0;

            action BoxAppears when (box_up == 0 && pending == 0) {
                box_up = 1;
            }
            action BoxLeaves when (box_up == 1) {
                box_up = 0;
            }
            action WallAppears when (wall == 0 && box_up == 0 && pending == 0) {
                wall = 1;
            }
            action WallDue when (wall == 1) {
                wall = 2;
            }
            // (Not while an unseen point's deadline runs: that clock is
            // the deadline's, `TimePassesUnseen`.)
            action HourPasses when (used > 0 && wall == 0 && pending <= 2) {
                used = 0;
            }
            // Someone else's turn of real work: the streak ends.
            action HumanWork when (pending == 0 && box_up == 0 && wall == 0) {
                short = 0;
            }
            action Continue when (
                pending == 0 && (box_up == 0 || Buggy == 1) && wall == 0 &&
                used <= Budget - 1 && short <= 1
            ) {
                typed_blocked = if box_up == 1 || wall == 1 { 1 } else { typed_blocked };
                typed_after_done = if short > 1 { 1 } else { typed_after_done };
                used = used + 1;
                pending = 1;
            }
            action WorkedShort when (pending == 1) {
                pending = 0;
                short = short + 1;
            }
            action WorkedLong when (pending == 1) {
                pending = 0;
                short = 0;
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
                short = if Buggy == 1 { short } else { short + 1 };
                waited = 0;
            }
            action Retry when (
                pending == 0 && (box_up == 0 || Buggy == 1) && wall == 2 &&
                tries <= Tries - 1 && used <= Budget - 1
            ) {
                typed_blocked = if box_up == 1 || wall == 1 { 1 } else { typed_blocked };
                used = used + 1;
                tries = tries + 1;
                pending = 2;
            }
            action RetryTaken when (pending == 2) {
                pending = 0;
                wall = 0;
                tries = 0;
            }
            action RetryHitsTheWall when (pending == 2) {
                pending = 0;
                wall = 1;
            }

            invariant NeverTypeUnderABoxOrAWallsWait: typed_blocked == 0;
            invariant WithinTheBudget: used <= Budget;
            invariant TwoShortContinuationsEscalate: typed_after_done == 0;
            invariant NoSilentLatch: latched == 0;
        }
    }
}
