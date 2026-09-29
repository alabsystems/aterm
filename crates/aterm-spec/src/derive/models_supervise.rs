// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's own bounded machines (aterm-agent `supervise`, lane B2):
//! the session claim it acts under, the focus-then-Enter choice it makes on
//! an unnumbered dialog, the turn-end policy and its done check, the keys it
//! sends to answer Claude Code's question dialog, the keystrokes of a
//! decline, when it types into an API error the network caused, and the
//! stall's remedy.

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
/// `0`, the default, is none). And NOTHING — no continuation, no retry, no
/// draft sent, no escalation — goes into a session nobody has asked anything
/// (D1 of the live E2E of 2026-09-26): the harness's OWN typed turns (its
/// upgrade notice, its carry-on, and what the supervisor itself typed, by
/// its ledger) are no task, however much the agent works on them.
///
/// `asked` 1 once a person or an orchestrator asked the session something
/// and its agent answered (`HumanWork`: their turn of real work); `tasked`
/// what the policy reads of it (`aterm_agent::harness::upgrade::TaskScan`
/// over the conversation's record, beside the screen's launch card) — equal
/// to `asked` in the shipped policy; `HarnessTurn` the harness's own turn —
/// the upgrade's notice or its carry-on, typed by the session's host into a
/// session with a task or without one (its record marked, beside the
/// supervisor's own turns by its ledger) — answered SHORT: NO TASK, and NO
/// SHORT TURN OF THE WORKER'S — the streak and its back-off stand as the
/// worker's own turns left them (N1 of the live E2E of 2026-09-26: the READY
/// answer and the carry-on's reply, read as someone else's short turns,
/// backed a finished stage's continuation off 4 minutes); `HarnessTurnLong`
/// the same turn answered with REAL WORK (a carry-on's answer is the
/// worker's own work, resumed): still no task, and the streak ends as after
/// any turn of real work (the review of the N1 fix: a 20-minute carry-on
/// answer was backed off like the first point before it). `harness` counts
/// those turns, up to the upgrade's two; `earned` is 1 while a back-off the
/// WORKER's turns earned runs — the back-off's ghost, which no harness turn
/// sets (`NeverBackOffForTheHarness`); `long` is 1 while the last turn
/// answered did real work — anyone's, the harness's included — after which
/// no back-off runs (`NeverBackOffAfterRealWork`).
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
/// reports done" after two short turns (the rule until 2026-09-24), keeps
/// awaiting an unseen point past its deadline (the silent latch lane B2's
/// review found), reads the harness's own turn as a task (the E2E's D1:
/// `keep going` and `answer_text` into a session nobody had asked anything),
/// as a short turn of the worker's (N1), and its answer of real work as no
/// end of the streak (the N1 fix's review) — each caught. Tier-1
/// (aterm-agent `tests/supervise_conformance_turn_end.rs`) drives the real
/// decider along every reachable state of this model, with no cap and with
/// one, and checks that it types exactly where the model allows it, waits
/// where a wall, a person or a back-off holds it, is never silent at a free
/// point of a session with a task, and types nothing into one without —
/// its `tasked` the REAL `TaskScan`'s over the record the walk writes (a
/// person's prompt for `HumanWork`, the harness's marked one and the
/// supervisor's ledgered `keep going` for `HarnessTurn`) — and its streak
/// the real one, a `HarnessTurn` the host's typed turn
/// (`TurnEndState::host_typed`) answered short, a `HarnessTurnLong` the same
/// answered with real work.
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
            var asked = 0;
            var tasked = 0;
            var typed_taskless = 0;
            var harness = 0;
            var earned = 0;
            var long = 0;

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
                earned = 0;
            }
            // Someone else's turn of real work (a draft among it sent): the
            // streak ends — and the session has a task (their prompt,
            // answered).
            action HumanWork when (pending == 0 && box_up == 0 && wall == 0) {
                short = 0;
                backoff = 0;
                earned = 0;
                long = 1;
                draft = 0;
                asked = 1;
                tasked = 1;
            }
            // The harness's own turn — its notice or its carry-on, the
            // upgrade's two — answered short: NO task, and no short turn of
            // the worker's: the streak and its back-off stand.
            action HarnessTurn when (
                harness <= 1 && pending == 0 && box_up == 0 && wall == 0 && draft == 0
            ) {
                harness = harness + 1;
                short = if Buggy == 1 && short <= Streak - 1 { short + 1 } else { short };
                backoff = if Buggy == 1 { 1 } else { backoff };
                tasked = if Buggy == 1 { 1 } else { tasked };
            }
            // …answered with REAL WORK (a carry-on's answer is the worker's
            // own work, resumed): still no task, and the streak ends.
            action HarnessTurnLong when (
                harness <= 1 && pending == 0 && box_up == 0 && wall == 0 && draft == 0
            ) {
                harness = harness + 1;
                short = if Buggy == 1 { short } else { 0 };
                backoff = if Buggy == 1 { backoff } else { 0 };
                earned = if Buggy == 1 { earned } else { 0 };
                long = 1;
                tasked = if Buggy == 1 { 1 } else { tasked };
            }
            action Continue when (
                pending == 0 && wall == 0 && (tasked == 1 || Buggy == 1) &&
                ((box_up == 0 && person == 0 && draft == 0 && backoff == 0) || Buggy == 1) &&
                (Budget == 0 || used <= Budget - 1)
            ) {
                typed_taskless = if asked == 0 { 1 } else { typed_taskless };
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
                earned = 1;
                long = 0;
            }
            action WorkedLong when (pending == 1) {
                pending = 0;
                short = 0;
                backoff = 0;
                earned = 0;
                long = 1;
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
                earned = if Buggy == 1 { earned } else { 1 };
                long = if Buggy == 1 { long } else { 0 };
                waited = 0;
            }
            action Retry when (
                pending == 0 && wall == 2 && (tasked == 1 || Buggy == 1) &&
                ((box_up == 0 && person == 0 && draft == 0) || Buggy == 1) &&
                (Budget == 0 || used <= Budget - 1)
            ) {
                typed_taskless = if asked == 0 { 1 } else { typed_taskless };
                typed_blocked = if box_up == 1 || person == 1 { 1 } else { typed_blocked };
                typed_over = if draft == 1 { 1 } else { typed_over };
                used = if Budget == 0 { used } else { used + 1 };
                pending = 2;
            }
            // The draft sent as it stands, in the act's place: a
            // continuation's turn, or at a wall whose wait is over its
            // retry's.
            action SubmitDraft when (
                pending == 0 && draft == 1 && (wall == 0 || wall == 2) && tasked == 1 &&
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
                pending == 0 && box_up == 0 && person == 0 && tasked == 1 && (
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
            invariant NeverTypeIntoATasklessSession: typed_taskless == 0;
            invariant NeverBackOffForTheHarness: backoff <= earned;
            invariant NeverBackOffAfterRealWork: backoff == 0 || long == 0;
        }
    }
}

/// THE DONE CHECK (aterm-agent `supervise/policy/turn_end.rs`; decided
/// 2026-09-27 under the owner's standing direction). A worker whose last
/// words say it is DONE is not continued: at the point past its back-off it
/// is typed the check — *"If the task is finished and verified, reply DONE
/// and stop; otherwise continue"* — and when that check's own yield says
/// done again THE TASK IS DONE: nothing more is typed into the session until
/// someone else's turn (a person's, an orchestrator's) opens it again. The
/// live E2E of 2026-09-26 (its C) shows what the policy before this did: a
/// finished task continued on the back-off until a small model invented
/// work with the owner's identity on it.
///
/// `done` is the done reports in a row at the points the policy saw: 0 none,
/// 1 the point is one (the check is owed), 2 the task is done; `pending` 0
/// at a point, 1 a continuation's turn running, 2 the check's, 3 a wall's
/// act's (the 529's retry, the limit's resume, `/compact`); `walled` the
/// point shows a wall — its ladder acts there, never the continuation or the
/// check. `typed_done` something was typed into a done task, `unchecked` a
/// done report was answered with a plain continuation, `cut_short` the task
/// ended on a done report the check never asked about. `Buggy = 1` is the
/// policy before 2026-09-27 — a done report continued like any point, and
/// read as the end the moment it repeats — and the check's first cut, whose
/// done task someone else's turn that ended on a wall left closed (the
/// review of 2026-09-27: a person's turn that hit a 529 after the task was
/// done was stranded there, no retry) — caught by each invariant on a path
/// of its own. Tier-1 (aterm-agent `tests/supervise_conformance_done_check.rs`)
/// drives the real `decide_turn_end` and `TurnEndState` along every
/// reachable state and checks it types the continuation, the check, the
/// wall's act, or nothing exactly where the model does.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_done_check_model() -> Model {
    crate::ty_model! {
        SupervisorDoneCheck {
            const Buggy = 0;
            var done = 0;
            var pending = 0;
            var typed_done = 0;
            var unchecked = 0;
            var cut_short = 0;
            var walled = 0;

            // Someone else's turn, answered: the task is theirs again, and
            // a done report in it is a first one — and one that ended on a
            // wall is no done report at all.
            action OtherTurnDone when (pending == 0) {
                done = 1;
                walled = 0;
            }
            action OtherTurnWork when (pending == 0) {
                done = 0;
                walled = 0;
            }
            action OtherTurnWall when (pending == 0) {
                done = if Buggy == 1 { done } else { 0 };
                walled = 1;
            }
            // The policy's act at a free point: the wall's act at a wall,
            // the continuation where no done was reported, the check where
            // one was, nothing on a done task.
            action WallAct when (pending == 0 && walled == 1 && done <= 1) {
                walled = 0;
                pending = 3;
            }
            action Continue when (pending == 0 && walled == 0 && (done == 0 || Buggy == 1)) {
                typed_done = if done == 2 { 1 } else { typed_done };
                unchecked = if done == 1 { 1 } else { unchecked };
                pending = 1;
            }
            action Check when (pending == 0 && walled == 0 && done == 1) {
                pending = 2;
            }
            action YieldWork when (pending > 0) {
                pending = 0;
                done = 0;
            }
            action YieldDone when (pending > 0) {
                cut_short = if pending == 1 && done == 1 { 1 } else { cut_short };
                done = if pending == 2 || (Buggy == 1 && done == 1) { 2 } else { 1 };
                pending = 0;
            }
            // A yield that ended on a wall: its done reports stand.
            action YieldWall when (pending > 0) {
                pending = 0;
                walled = 1;
            }

            invariant NeverTypeIntoADoneTask: typed_done == 0;
            invariant ADoneReportIsChecked: unchecked == 0;
            invariant OnlyTheCheckEndsTheTask: cut_short == 0;
            invariant AWallIsNeverLeftOnADoneTask: walled == 0 || done <= 1;
        }
    }
}

/// THE STALL'S REMEDY (aterm-agent `supervise/stall.rs`,
/// `Session::stall_remedy`; decided 2026-09-27 under the owner's standing
/// direction, D4, and cut back by the review of that day). A worker the
/// server says is frozen is ended with `signal term`, for its relaunch, only
/// with nobody there and its bound passed — and ONCE an episode, never a
/// kill: a program that outlives the term keeps the server's attention,
/// whose `signal kill` is a person's.
///
/// `stalled` 0 the worker reads its input, 1 frozen (`input=stalled`), 2 a
/// stopped job (`input=stopped`: its `signal cont` is a person's); `aged`
/// the stall has stood `[harness] stall_term_after_s`; `off` that bound is
/// `0`; `hand` a person's hand within `human_grace_s`, a lease or a named
/// turn (`hand=`); `host` the loop's host relaunches an agent the remedy
/// ends (`relaunch`); `termed` this episode's term is sent; `terms` the
/// terms sent this episode; `wrong` a term sent under a hand; `killed` a
/// kill sent. `Term` is the remedy; every other action is the world.
/// `Buggy = 1` adds three dead actions, each caught alone by an invariant of
/// its own: the first cut's kill a minute after the term (`Kill`), a term
/// blind to a person's hand (`TermBlind`), and one that forgets it already
/// signalled (`TermAgain`). Tier-1 (aterm-agent `supervise/stall_tests.rs`,
/// `tier1_the_real_remedy_signals_exactly_where_the_model_terms`) drives the
/// real loop over a frozen worker in every reachable configuration and
/// checks it sends `signal term` exactly where `Term` is enabled, once.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_stall_remedy_model() -> Model {
    crate::ty_model! {
        SupervisorStallRemedy {
            const Buggy = 0;
            var stalled = 0;
            var aged = 0;
            var off = 0;
            var hand = 0;
            var host = 1;
            var termed = 0;
            var terms = 0;
            var wrong = 0;
            var killed = 0;

            action Freeze when (stalled == 0) {
                stalled = 1;
            }
            action Stop when (stalled == 0) {
                stalled = 2;
            }
            action Age when (stalled > 0 && aged == 0) {
                aged = 1;
            }
            action Thaw when (stalled > 0) {
                stalled = 0;
                aged = 0;
                termed = 0;
                terms = 0;
            }
            action HandOn when (hand == 0) {
                hand = 1;
            }
            action HandOff when (hand == 1) {
                hand = 0;
            }
            action HostFlips when (termed == 0) {
                host = if host == 1 { 0 } else { 1 };
            }
            action BoundFlips when (termed == 0) {
                off = if off == 1 { 0 } else { 1 };
            }
            action Term when (stalled == 1 && aged == 1 && off == 0 && host == 1
                && hand == 0 && termed == 0) {
                terms = terms + 1;
                termed = 1;
            }
            // The remedies it could have been, each dead at `Buggy = 0`.
            action TermBlind when (Buggy == 1 && stalled == 1 && aged == 1 && off == 0
                && host == 1 && hand == 1 && termed == 0) {
                wrong = 1;
                terms = terms + 1;
                termed = 1;
            }
            action TermAgain when (Buggy == 1 && stalled == 1 && termed == 1 && terms <= 1) {
                terms = terms + 1;
            }
            action Kill when (Buggy == 1 && termed == 1 && stalled == 1) {
                killed = 1;
            }

            invariant NeverUnderAHand: wrong == 0;
            invariant OneTermAnEpisode: terms <= 1;
            invariant NeverAKill: killed == 0;
        }
    }
}

/// A TURN END THE HOST LET GO IS DECIDED AGAIN (aterm-agent
/// `supervise/run.rs`, `Session::host_held` and `wait_for_next`; D3 of the
/// live E2E of 2026-09-26). The loop decides each point once, as it comes;
/// the session's host (the window's worker, `IdleHost`) may own the
/// session's turn ends meanwhile (`IdleHost::owns_turn_end`: the live
/// upgrade's wind-down) and may take its own step at the point, in the
/// point's act's place. Once the host owns nothing any more, the point it was
/// left to is DECIDED AGAIN — never waited on with nobody deciding it (the
/// E2E's Stage-1 end sat five minutes after the upgrade's settle ownership
/// lapsed).
///
/// `point` 0 while the agent works, 1 a turn end the loop decided (its act
/// taken, or its wait running), 2 one left to the host — decided while it
/// owned the turn ends (`TurnEnds` under `owns`), or passed over for a step
/// that typed nothing and ended nothing (`HostKeeps`: one that owns the turn
/// ends after it; `HostLetsGo`: one that owns nothing, a wait past its bound
/// or a last word) — and 3 one a host step MOVED (`HostMoves`: it typed into
/// the agent, a notice or a carry-on, or ended it; the screen may still show
/// the old point until the agent draws its answer, but the point is gone and
/// the turn it started is under way); `owns` the host owns the session's
/// turn ends (`Lapse`: switched off, or let go with no step); `stranded` the
/// loop waited on a point left to a host that owns nothing; `stale` the loop
/// decided a point a host step had moved (the review of 2026-09-26: the
/// point after a typed carry-on was decided again at once, while the agent
/// began its answer). `Redecide` is the loop's decision again; `LoopWaits`
/// its wait for the screen to move. `Buggy = 1` is the loop that decides
/// each point once and waits on it whoever owns it, and that decides again
/// whatever a step left behind — both caught. Tier-1 (aterm-agent
/// `supervise/run_engine_tests.rs`, `a_turn_end_the_host_let_go_is_decided_again`)
/// drives the real loop along the model's paths and checks it continues
/// exactly where `Redecide` is enabled.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_host_turn_end_model() -> Model {
    crate::ty_model! {
        SupervisorHostTurnEnd {
            const Buggy = 0;
            var point = 0;
            var owns = 0;
            var stranded = 0;
            var stale = 0;

            action TurnEnds when (point == 0) {
                point = if owns == 1 { 2 } else { 1 };
            }
            action HostKeeps when (point == 1 || point == 2) {
                owns = 1;
                point = 2;
            }
            action HostLetsGo when (point == 1 || point == 2) {
                owns = 0;
                point = 2;
            }
            action HostMoves when (point == 1 || point == 2) {
                point = 3;
            }
            action Lapse when (owns == 1) {
                owns = 0;
            }
            action Redecide when (owns == 0 && (point == 2 || (point == 3 && Buggy == 1))) {
                stale = if point == 3 { 1 } else { stale };
                point = 1;
            }
            action LoopWaits when (point > 0 && (point == 1 || point == 3 || owns == 1 || Buggy == 1)) {
                stranded = if point == 2 && owns == 0 { 1 } else { stranded };
            }
            action TurnRuns when (point > 0) {
                point = 0;
            }

            invariant NoTurnEndLeftToNobody: stranded == 0;
            invariant NeverDecideAPointAStepMoved: stale == 0;
        }
    }
}

/// A PERSON'S GRACE OVER A READY ANSWER ENDS IN THE UPGRADE'S RESTART, NEVER IN
/// THE LOOP'S CONTINUATION (aterm-agent `harness/upgrade_drive.rs`,
/// `owns_turn_ends`, `attended_owned` and `after_attended`; the window's host
/// `WorkerIdle::at_idle` and its `UpgradeRun` counts; the loop's
/// `decide_turn_end` under `TurnEndReading::upgrading`). The live point of
/// 2026-09-27 20:27 (s-d3346): the agent answered READY at 20:27:20; the owner had
/// typed "ok" at 20:26:42, so the upgrade's restart waited on the person
/// (`held-back:attended`, then `wait:attended`) — owning none of the session's turn
/// ends, and looking again on its growing ladder. The loop held off for the same
/// person's grace, and at its lapse (20:28:42) nobody owned the point but the loop:
/// it typed `keep going` over the READY, 28 s before the upgrade's next look, and
/// the upgrade then waited 5 h 50 min for an idle point of an agent that was never
/// idle again.
///
/// Before the READY (`point` 0) the host looks at the agent's other turn ends: an
/// answer without READY (`HostWaitsBefore`, `wait:awaiting-ready`) or a re-ask a
/// person at the tab holds (`HostHoldsBefore`, `wait:attended`). The READY comes
/// with a turn of the agent's (`ReadyArrives`), and its point stands (`point` 1)
/// until acted on (`point` 2). The host steps first at a point (`HostLooks`: the
/// loop takes the host's step before its own decision, `looked`); a look that
/// finds the person's grace running (`person` 1) waits `attended` and owns the
/// turn ends while its backstop count (`count`) is within `Bound` — the backstop
/// for a stamp that fails closed; one that finds it lapsed takes the restart
/// (`HostActs`). The person may touch the tab again (`PersonTouches`) and the
/// grace runs out (`GraceLapses`); the loop continues the agent (`LoopContinues`)
/// only where nobody's grace holds and the host owns nothing. `here` is the
/// attended looks at the READY's point — the spec's own measure of the backstop,
/// whatever the host counts — and `over`: the loop continued over the READY while
/// that had not run out.
///
/// The host's count is the attended looks in a row at one point: any other wait
/// starts it over, and so does the READY's own turn. Three defects, each caught
/// on its own dial: `Buggy = 1`, the host before 144d9547a (an attended wait owns
/// nothing); `Every = 1`, its first cut (the backstop read every wait in a row
/// since the last act — the notice's `awaiting-ready` turn ends among them — so a
/// READY after enough of them owned nothing at its first look, the review of
/// 2026-09-28); `Stretch = 1`, a count the READY's turn does not start over (a
/// person who held the re-ask past the backstop left the READY to the loop).
///
/// Tier-1: aterm-gui's `harness_host::tests::a_person_at_the_tab_leaves_the_point_to_the_upgrade_for_their_grace`
/// drives the real worker along `HostWaitsBefore`/`HostHoldsBefore`,
/// `ReadyArrives` (its `turn_ran`) and `HostLooks` (its `owns` projected at every
/// look, `Bound` set to the real backstop's looks), and aterm-agent's
/// `supervise::policy::turn_end` test `a_point_the_upgrade_owns_is_never_continued_at_the_grace_lapse`
/// binds `LoopContinues`' guard to the real turn-end policy.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn upgrade_attended_turn_end_model() -> Model {
    crate::ty_model! {
        UpgradeAttendedTurnEnd {
            const Buggy = 0;
            const Every = 0;      // 1: the backstop reads every wait in a row
            const Stretch = 0;    // 1: the READY's turn does not start the count over
            const Bound = 3;      // the attended looks the backstop owns the point for
            const Looks = 7;      // run bound: looks and touches
            var point = 0;
            var person = 1;
            var owns = 0;
            var looked = 0;
            var count = 0;
            var waits = 0;
            var here = 0;
            var over = 0;
            var steps = 0;

            action HostWaitsBefore when (point == 0 && steps <= Looks) {
                steps = steps + 1;
                waits = waits + 1;
                count = 0;
            }
            action HostHoldsBefore when (point == 0 && person == 1 && steps <= Looks) {
                steps = steps + 1;
                waits = waits + 1;
                count = count + 1;
            }
            action ReadyArrives when (point == 0) {
                point = 1;
                count = if Stretch == 1 { count } else { 0 };
            }
            action HostLooks when (point == 1 && person == 1 && steps <= Looks) {
                steps = steps + 1;
                looked = 1;
                owns = if Buggy == 1 {
                    0
                } else {
                    if Every == 1 {
                        if waits <= Bound { 1 } else { 0 }
                    } else {
                        if count <= Bound { 1 } else { 0 }
                    }
                };
                count = count + 1;
                waits = waits + 1;
                here = here + 1;
            }
            action HostActs when (point == 1 && person == 0) {
                looked = 1;
                owns = 0;
                point = 2;
            }
            action PersonTouches when (point <= 1 && person == 0 && steps <= Looks) {
                steps = steps + 1;
                person = 1;
            }
            action GraceLapses when (person == 1) {
                person = 0;
            }
            action LoopContinues when (point == 1 && looked == 1 && person == 0 && owns == 0) {
                over = if here <= Bound + 1 { 1 } else { over };
                point = 2;
            }

            invariant NeverContinueOverAReadyHeldByAPerson: over == 0;
        }
    }
}

/// THE LOOP'S OWN ACT, ONCE THE AGENT HAS TAKEN IT, NEVER WITHHOLDS A BREAK
/// FROM ITS HOST (aterm-agent `supervise/run.rs`,
/// `Session::host_steps_in_background` and the live-turn read in
/// `await_turn_from`; `supervise/policy/turn_end.rs`,
/// `TurnEndState::act_untaken` and `took`). The live upgrade's notice, re-ask,
/// give-up and release reach an agent that orchestrates all day only at the
/// BREAKS of its own background work (`IdleHost::at_background`); the loop
/// withholds a break while an act of its own is in the way. The guard asked
/// whether the act's POINT had been judged, and only a true idle point judges
/// one — a break reads busy. So behind the loop's own `keep going` every break
/// was withheld until an idle point that an always-busy agent never reaches:
/// on 2026-09-28 s-d3346 (from 14:52:45, the owner's `--now` standing) and
/// s-5c03a (from 18:21:04, over its READY) had no look of the upgrade's for
/// hours, every clock of it frozen with them.
///
/// `screen` 0 an idle point, 1 a read of the agent's LIVE TURN (its spinner,
/// its busy footer), 2 a settled break of its own background work; `act` 0
/// none of the loop's awaited, 1 typed and not taken (no live turn read since:
/// it may sit queued behind work the reader missed), 2 taken, its point not
/// judged yet. `Offer` is the loop handing the break to its host, `Withhold`
/// its refusal. Two ghosts: `stacked`, a break offered over an act the agent
/// has not taken (the guard's purpose — no line of the host's stacked on a
/// continuation the agent has not read); `withheld`, a break refused after the
/// act was taken (the incident). ONE DIAL PER DEFECT, each caught alone:
/// `Judged = 1` the guard of 0.98 and main to 2026-09-28 (the act's point
/// judged); `Blind = 1` a guard that asks nothing of the act. `Buggy = 1` both.
///
/// Tier-1: aterm-agent's `supervise/run_engine_tests.rs`
/// (`a_break_after_the_loops_own_taken_act_is_offered_to_the_host`) drives the
/// real loop over scripted screens along the model's paths — the loop's `keep
/// going`, a live turn or none, a settled break — and the real loop offers the
/// break exactly where `Offer` is enabled.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_break_offer_model() -> Model {
    crate::ty_model! {
        SupervisorBreakOffer {
            const Buggy = 0;
            const Judged = 0;     // 1: the guard asks for the act's point judged (0.98)
            const Blind = 0;      // 1: the guard asks nothing of the act
            const Acts = 2;       // run bound: the loop's acts
            var screen = 0;
            var act = 0;
            var acts = 0;
            var stacked = 0;
            var withheld = 0;

            // The loop types its act at an idle point (`keep going`).
            action LoopActs when (screen == 0 && act == 0 && acts <= Acts - 1) {
                act = 1;
                acts = acts + 1;
            }
            // A read of the agent's live turn: the act it follows is taken.
            action LiveTurn when (screen == 0 || screen == 2) {
                screen = 1;
                act = if act == 1 { 2 } else { act };
            }
            // A break of the agent's own work stands its settle: after a live
            // turn, or straight after the act (queued behind work the reader
            // did not see).
            action BreakSettles when (screen <= 1) {
                screen = 2;
            }
            // A point: the act awaited is judged there.
            action PointComes when (screen > 0) {
                screen = 0;
                act = 0;
            }
            // The loop's guard at a settled break: the host is offered it.
            action Offer when (
                screen == 2 && (act == 0 || act == 2 || Blind == 1 || Buggy == 1) && (act <= 1 || Judged == 0)
            ) {
                stacked = if act == 1 { 1 } else { stacked };
            }
            // Or refused it.
            action Withhold when (
                screen == 2 && (act == 1 || (act == 2 && (Judged == 1 || Buggy == 1)))
            ) {
                withheld = if act == 2 { 1 } else { withheld };
            }

            invariant NeverStackOnAnUntakenAct: stacked == 0;
            invariant NoBreakWithheldAfterTheActWasTaken: withheld == 0;
        }
    }
}

/// THE POINT THE UPGRADE ACTS AT NEXT IS OWNED THROUGH ITS SETTLE (aterm-gui
/// `harness_host.rs`, `UpgradeRun` and `WorkerIdle::at_idle`/`at_background`;
/// aterm-agent `harness/upgrade_drive.rs`, `owns_turn_ends` and
/// `bounded_wait`). The restart after a READY waits out a short settle first
/// (`wait:settling`: Claude's status and the screen quiet `QUIET_S`), and the
/// upgrade owns the session's turn ends through it for `OWNED_SETTLE_LOOKS`
/// looks, so the loop types nothing over the point it is about to act at. What
/// bounds that ownership is the host's count of the waits it bounds — the
/// settle's and the drain's — since its last ACT: an act starts it over, and a
/// turn of the agent's never does (68c98fd18: a self-waking agent must not
/// renew it). The incident (2026-09-28, s-5c03a): six looks at a person at the
/// tab from 14:48, then three notices typed at BREAKS (16:54, 17:26, 17:56)
/// that never went through the count, then the READY at 18:21:02 — whose first
/// look (`wait:settling`) read those six waits and owned nothing, and the loop
/// typed `keep going` over the READY one second later. The READY was void, and
/// the agent was never idle again.
///
/// `point` 0 before the READY, 1 the READY's point standing, 2 acted on (the
/// restart) or taken by the loop; `noticed` a notice is out; `waits` the host's
/// count; `looks` its looks at the READY's point; `owns` what its last look
/// left. Before the notice, settle waits at idle points (`SettleBefore`,
/// counted); notices at an idle point or at a break (`NoticeAtIdle`,
/// `NoticeAtBreak`: acts); and every other wait (`OtherWait`: a person at the
/// tab, an answer without READY, a resting round — each owns nothing, and no
/// settle or drain of the restart's). `over`: the loop continued over the
/// READY while its settle still had looks to run. DIALS, each caught alone:
/// `Carry = 1` a notice typed at a break leaves the count standing (0.98, and
/// main to 2026-09-28); `Every = 1` every wait counts (the same builds: the
/// notice's `awaiting-ready` answers and a person's looks spent the settle's
/// looks before the READY came). `Buggy = 1` both.
///
/// Out of the model, stated: a RE-ASK whose own settle outlived its owned
/// looks at a point the loop then took — its settle waits stand, and a READY
/// to the notice before it owns nothing at its first look; the next re-ask, an
/// act, starts the count over.
///
/// Tier-1: aterm-gui's
/// `harness_host::tests::a_ready_after_notices_at_breaks_owns_its_settle`
/// drives the real worker along the incident's path and the model's.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn upgrade_settle_turn_end_model() -> Model {
    crate::ty_model! {
        UpgradeSettleTurnEnd {
            const Buggy = 0;
            const Carry = 0;      // 1: a notice at a break leaves the count standing
            const Every = 0;      // 1: every wait counts
            const Bound = 2;      // OWNED_SETTLE_LOOKS
            const Steps = 5;      // run bound
            var point = 0;
            var noticed = 0;
            var waits = 0;
            var looks = 0;
            var owns = 0;
            var over = 0;
            var steps = 0;

            action SettleBefore when (point == 0 && noticed == 0 && steps <= Steps - 1) {
                steps = steps + 1;
                waits = waits + 1;
            }
            action OtherWait when (point == 0 && steps <= Steps - 1) {
                steps = steps + 1;
                waits = if (Every == 1 || Buggy == 1) { waits + 1 } else { waits };
            }
            action NoticeAtIdle when (point == 0 && steps <= Steps - 1) {
                steps = steps + 1;
                noticed = 1;
                waits = 0;
            }
            action NoticeAtBreak when (point == 0 && steps <= Steps - 1) {
                steps = steps + 1;
                noticed = 1;
                waits = if (Carry == 1 || Buggy == 1) { waits } else { 0 };
            }
            // The READY comes with a turn of the agent's: the count is kept.
            action ReadyArrives when (point == 0 && noticed == 1) {
                point = 1;
            }
            // A settle look at the READY's point: it owns the turn ends while
            // the count before it is within the bound.
            action HostSettles when (point == 1 && looks <= Bound) {
                owns = if waits <= Bound - 1 { 1 } else { 0 };
                waits = waits + 1;
                looks = looks + 1;
            }
            // Settled: the restart.
            action HostActs when (point == 1 && looks > 0) {
                owns = 0;
                point = 2;
            }
            action LoopContinues when (point == 1 && looks > 0 && owns == 0) {
                over = if looks <= Bound { 1 } else { over };
                point = 2;
            }

            invariant NeverContinueOverASettlingReady: over == 0;
        }
    }
}

/// A NOTICE ITS OWN FENCE REFUSED OWNS ITS POINT, BOUNDED (aterm-agent
/// `harness/upgrade_drive.rs`, `refused`, `owns_turn_ends`, `after_refused`,
/// `OWNED_REFUSED_LOOKS` and the notice's in-visit tries; aterm-gui
/// `harness_host.rs`, `UpgradeRun`'s own count of refused looks and
/// `UpgradeRun::worded`). The live point (2026-09-28, s-d3346): the owner's
/// `--now` at 14:50:50, and at the one idle point the agent offered the notice
/// was due — its gate said go — but the host's `turn` refused it (`if-gen=`
/// saw the screen move: `announce-refused:changed`) at 14:50:58, 14:51:19 and
/// 14:52:21, each a wait that owned nothing on a climbing ladder; at 14:52:45
/// the loop typed `keep going` into the point, and the agent was never idle
/// again. Each refused look now owns the session's turn ends while the refused
/// looks since the upgrade's last act or the owner's last word are within
/// `Bound`, and is looked at again at the first rung, so the notice is tried
/// again at the very point it is due; past the bound the point is the loop's.
/// A turn of the agent's never renews the count (68c98fd18): an act — the
/// notice typed — starts it over, and so does the owner's word.
///
/// `point` 1 the idle point standing, 0 the agent's turn; `due` the notice is
/// still to be typed; `count` the host's count its ownership is read by;
/// `refusals` the refused looks since the last act or word (what the count
/// must be); `owns` what its last look left. `Refused` is a look whose fence
/// refused, `Types` one whose notice went (an act: `owns` 1, the notice's own
/// claim), `SettleBefore` a look whose notice gate settled (its own bound is
/// the settle machine's), `OwnerWord` the owner's word or a newer build (the
/// host's activation notice: the worker looks again). `over`: the loop
/// continued at a point whose refused notice still had owned looks to run.
/// `Buggy = 1`: every defect below at once, caught. DIALS, each caught alone
/// by the bounded check: `Shared = 1` settle waits and refused looks share one
/// count (the first build of this bound: a round whose gate had settled
/// `Bound` looks owned nothing at its first refusal); `Stale = 1` the owner's
/// word leaves the count standing (the same build: an `Upgrade now` pressed
/// after the ninety minutes of refusals the band tells of owned nothing).
/// THE BOUND IS A PATH, NOT AN INVARIANT: `Renew = 1` (a turn of the agent's
/// starts the count over, so every point is owned anew, against 68c98fd18)
/// keeps the invariant — it owns more, not less — and the Tier-0 test pins the
/// bound's path instead: `Bound` refused looks, a turn, a refused look that
/// owns nothing, where `Renew` owns it.
///
/// Tier-1: aterm-gui's
/// `harness_host::tests::a_refused_notice_owns_its_point_for_its_looks` drives
/// the real worker along the incident's path — refused looks, a turn, refused
/// looks again — and along settles before the refusals and the owner's word
/// after them, beside the model's.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn upgrade_refused_turn_end_model() -> Model {
    crate::ty_model! {
        UpgradeRefusedTurnEnd {
            const Buggy = 0;
            const Renew = 0;      // 1: a turn of the agent's starts the count over
            const Shared = 0;     // 1: settle waits and refused looks share one count
            const Stale = 0;      // 1: the owner's word leaves the count standing
            const Bound = 3;      // OWNED_REFUSED_LOOKS
            const Steps = 8;      // run bound
            var point = 1;
            var due = 1;
            var count = 0;
            var refusals = 0;
            var owns = 0;
            var looked = 0;
            var over = 0;
            var steps = 0;

            // The host looks at the point, and the notice's gate settles.
            action SettleBefore when (point == 1 && due == 1 && steps <= Steps - 1) {
                steps = steps + 1;
                looked = 0;
                owns = 0;
                count = if (Shared == 1 || Buggy == 1) { count + 1 } else { count };
            }
            // The host looks at the point, and the notice's fence refuses it.
            action Refused when (point == 1 && due == 1 && steps <= Steps - 1) {
                steps = steps + 1;
                looked = 1;
                owns = if (Buggy == 0 && count <= Bound - 1) { 1 } else { 0 };
                count = count + 1;
                refusals = refusals + 1;
            }
            // Or types it: an act, and the notice owns the turn ends.
            action Types when (point == 1 && due == 1) {
                looked = 1;
                due = 0;
                owns = 1;
                count = 0;
                refusals = 0;
            }
            // The owner's word (or a newer build): the round is new, and the
            // worker looks again before the loop may take the point.
            action OwnerWord when (due == 1 && steps <= Steps - 1) {
                steps = steps + 1;
                looked = 0;
                owns = 0;
                count = if (Stale == 1 || Buggy == 1) { count } else { 0 };
                refusals = 0;
            }
            // The loop continues the agent at a point the host owns nothing of.
            action LoopContinues when (point == 1 && looked == 1 && owns == 0) {
                over = if (due == 1 && refusals <= Bound) { 1 } else { over };
                point = 0;
            }
            // The agent's turn ends: a new point, the count kept.
            action PointComes when (point == 0 && steps <= Steps - 1) {
                steps = steps + 1;
                point = 1;
                looked = 0;
                owns = 0;
                count = if Renew == 1 { 0 } else { count };
            }

            invariant NeverContinueOverARefusedNoticeInItsLooks: over == 0;
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
/// **The person fence** (`PersonFence`, R3b, built 2026-09-27): the loop
/// reads the person's stamp on the read the key is decided on
/// (`seen_quiet`), and the key names that read's person count (`key
/// if-human=`), which the server checks under the same lock as the write —
/// so a person's key written in the round trip since the read (`touched`:
/// `PersonKey` sets it, a `Read` clears it) skips the supervisor's. Until
/// then this was an ASSUMPTION (`QuietAtWrite`); at `PersonFence = 0` — a
/// host that sends no count — the invariants are refuted, as they were.
///
/// **One ASSUMPTION, not a mechanism** — a constant at 1 here, refuted at 0
/// by `aterm-spec/tests/derived_supervise.rs`, so the model says exactly
/// what it rests on:
///
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
            const PersonFence = 1;
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
            var touched = 0;
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
                touched = 1;
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
                touched = 0;
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
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0) &&
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
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0) &&
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
/// **The person fence** (`PersonFence`, R3b of the question answer, built
/// 2026-09-27): the loop reads the person's stamp on the read the keystroke
/// is fenced on (`seen_quiet`), and the keystroke names that read's person
/// count (`key|send if-human=`), checked under the server's lock with the
/// write — so a person's key written in the round trip since the read
/// (`touched`) skips the supervisor's. Until then an ASSUMPTION
/// (`QuietAtWrite`); at `PersonFence = 0` — a host that sends no count — it
/// is refuted by `aterm-spec/tests/derived_supervise.rs`, as it was.
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
            const PersonFence = 1;
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
            var touched = 0;
            var mine = 0;

            // ---- a person writes a key (its effect is its consume's) ----
            action PersonKey when (gone == 0 && q2 == 0) {
                q1 = if q1 == 0 { 5 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 5 };
                quiet = 0;
                touched = 1;
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
                touched = 0;
                mine = if mine == 1 && focus == seen_focus && open == seen_open
                    && text == seen_text { 1 } else { 0 };
            }
            action HarnessDown when (
                fresh == 1 && q2 == 0 && seen_focus == 0 && seen_open == 0 &&
                (Buggy == 1 || (mine == 0 &&
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0)))
            ) {
                q1 = if q1 == 0 { 1 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 1 };
                mine = 1;
                fresh = 0;
            }
            action HarnessTab when (
                fresh == 1 && q2 == 0 && seen_focus == 1 && seen_open == 0 &&
                (Buggy == 1 || (mine == 0 &&
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0)))
            ) {
                q1 = if q1 == 0 { 2 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 2 };
                mine = 1;
                fresh = 0;
            }
            action HarnessText when (
                fresh == 1 && q2 == 0 && seen_focus == 1 && seen_open == 1 && seen_text == 0 &&
                (Buggy == 1 || (mine == 0 &&
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0)))
            ) {
                q1 = if q1 == 0 { 3 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 3 };
                mine = 1;
                fresh = 0;
            }
            action HarnessEnter when (
                fresh == 1 && q2 == 0 && seen_focus == 1 && seen_open == 1 && seen_text == 1 &&
                (Buggy == 1 || (mine == 0 &&
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0)))
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

/// THE NETWORK WALL (aterm-agent `supervise/policy/turn_end.rs`,
/// `wall_action`'s arms for an API error that never reached the API and a
/// reply the connection cut off; the outage of 2026-09-27, when a worker's
/// `API Error: Can't reach the API server — check your internet or DNS
/// (ENOTFOUND)` was typed into sixteen times in an hour, 0.5 s after each
/// appearance): WHEN the supervisor types into such a wall, given what the
/// session's host MEASURES of the API's reach — and nothing else it may
/// believe.
///
/// The decision, as committed: a network never reached is tried on the
/// short ladder while its reach is not measured (`Rung1`, then `Rung2` for
/// every try after — the real 1, 2, 5 min, clamped at the last); it is
/// continued AT ONCE the first time the host measures the API reachable
/// again, and each later Up act is spaced by the Up acts before it (a
/// measure that lies, or an API that still fails, costs the ladder's own
/// count, never a burst); while the host measures it definitely DOWN
/// nothing is typed into it before `Hold` (the real `down_hold`, 15 min)
/// from its appearance, and one try is made then all the same, so a wrong
/// Down strands no one; a reply cut off is continued at once and then on
/// the same ladder, by the tries made — and under a Down it takes the
/// hold like an unreachable API. A try that led to real work before the
/// wall came back (the real `progress`) ends the EPISODE: its counts start
/// over (the six sleep cut-offs of 2026-09-26, each after 14–63 min of
/// work, are each continued at once).
///
/// `net` is the world (1 the agent's route works); `verdict` the host's
/// measure the loop reads (0 unknown — nobody measures, a custom route, a
/// stale measure, a timeout or a TLS failure; 1 down; 2 up), which moves
/// in EVERY direction, truthfully or not: `MeasureDown`/`MeasureUp` follow
/// the world, `MeasureLost` forgets it, `MeasureLies` says up while the
/// agent's route is down (a probe whose route works while the agent's does
/// not), `MeasureWrongDown` says down while it works — so a flapping or
/// lying probe is covered, not assumed away. `wall` is 0 while the worker
/// works, 1 while the wall shows (a point), 2 while the supervisor's act is
/// in flight; `cause` 0 unreachable, 1 cut off; `waited` the ticks since
/// this appearance (one tick is one minute at Tier-1, saturating at
/// `Hold`, the longest any verdict waits); `tries` and `ups` the episode's
/// acts and its acts on an Up measure (the real `attempts` and `ups`,
/// saturating where the ladder clamps). Ghosts: `quick` counts the
/// episode's acts made at the very appearance (`waited == 0`) — the day's
/// sixteen were all such; `dacts` the episode's acts typed while the host
/// measured the API down, and `dticks` the ticks the episode has spent at
/// its wall — neither read by any guard, and neither reset by an act.
///
/// The invariants, each about what the episode ACCUMULATES rather than
/// one decision (so neither is `Act`'s guard restated: a guard-level one —
/// "an act on a Down measure has `Hold <= waited`" — holds by construction
/// and would prove nothing, the review of 2026-09-27):
/// AMeasuredOutageIsTypedIntoAtMostOnceAHold — the k-th act typed into a
/// measured outage comes at least k holds of wall time into its episode
/// (k up to 2, saturating), however the measure flaps, lies or is lost in
/// between; it rests on the hold being measured from each APPEARANCE (an
/// act restarts `waited`), which `Carry = 1` breaks — the hold's clock
/// carried across the episode's appearances, as a decider keeping the
/// episode's first `since` would: after one hold, every re-appearance
/// under a Down measure is typed into at once — caught with `Act`'s guard
/// untouched. And AtMostTwoActsAtOncePerEpisode — an episode gets at most
/// one act at once for a reply cut off and one for the API measured back
/// (`AtOnce`); every other act waits at least a rung. `Buggy = 1` is the
/// supervisor of that day: it acts at once at every appearance, whatever
/// the measure — caught typing into a measured outage at once, and acting
/// at once a third time. Tier-1 (aterm-agent
/// `tests/supervise_conformance_network_wall.rs`) walks every reachable
/// state with the REAL `decide_turn_end` and `TurnEndState` in tow and
/// checks it types exactly where `Act` is enabled, under the rule and in
/// the words the model names, and otherwise waits until exactly the tick
/// `Act` becomes enabled; the `Buggy` model disagrees with it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_network_wall_model() -> Model {
    crate::ty_model! {
        SupervisorNetworkWall {
            const Buggy = 0;
            const Carry = 0;
            const Rung1 = 1;
            const Rung2 = 2;
            const Hold = 3;
            const AtOnce = 2;
            var net = 1;
            var verdict = 0;
            var wall = 0;
            var cause = 0;
            var waited = 0;
            var tries = 0;
            var ups = 0;
            var quick = 0;
            var dacts = 0;
            var dticks = 0;

            // ---- the world ----
            action NetFails when (net == 1) {
                net = 0;
            }
            action NetReturns when (net == 0) {
                net = 1;
            }

            // ---- the host's measure, in every direction ----
            action MeasureDown when (net == 0 && (verdict == 0 || verdict == 2)) {
                verdict = 1;
            }
            action MeasureUp when (net == 1 && (verdict == 0 || verdict == 1)) {
                verdict = 2;
            }
            action MeasureLost when (verdict == 1 || verdict == 2) {
                verdict = 0;
            }
            action MeasureLies when (net == 0 && (verdict == 0 || verdict == 1)) {
                verdict = 2;
            }
            action MeasureWrongDown when (net == 1 && (verdict == 0 || verdict == 2)) {
                verdict = 1;
            }

            // ---- the vendor gives up on a request: a wall, a new episode ----
            action Unreachable when (wall == 0 && net == 0) {
                wall = 1;
                cause = 0;
                waited = 0;
            }
            action CutOff when (wall == 0) {
                wall = 1;
                cause = 1;
                waited = 0;
            }

            // ---- time at the wall ----
            action Tick when (wall == 1 && waited <= Hold - 1) {
                waited = waited + 1;
                dticks = if dticks <= Hold + Hold - 1 { dticks + 1 } else { dticks };
            }

            // ---- the supervisor types its act ----
            action Act when (
                wall == 1 && (
                    Buggy == 1 ||
                    (cause == 0 && verdict == 2 &&
                        (ups == 0 || (ups == 1 && Rung1 <= waited) || (ups == 2 && Rung2 <= waited))) ||
                    (verdict == 1 && Hold <= waited) ||
                    (cause == 0 && verdict == 0 &&
                        ((tries == 0 && Rung1 <= waited) || (1 <= tries && Rung2 <= waited))) ||
                    (cause == 1 && (verdict == 0 || verdict == 2) &&
                        (tries == 0 || (tries == 1 && Rung1 <= waited) ||
                            (tries == 2 && Rung2 <= waited)))
                )
            ) {
                dacts = if verdict == 1 && dacts <= 1 { dacts + 1 } else { dacts };
                quick = if waited == 0 && quick <= AtOnce { quick + 1 } else { quick };
                ups = if cause == 0 && verdict == 2 && ups <= 1 { ups + 1 } else { ups };
                tries = if tries <= 1 { tries + 1 } else { tries };
                wall = 2;
                waited = if Carry == 1 { waited } else { 0 };
            }

            // ---- what the act led to ----
            // The vendor's own retries (minutes), then the wall again: the
            // episode's next appearance, its counts kept.
            action MetUnreachable when (wall == 2 && net == 0) {
                wall = 1;
                cause = 0;
            }
            action MetCutOff when (wall == 2) {
                wall = 1;
                cause = 1;
            }
            // Real work, then the wall again: a new episode.
            action WorkedThenUnreachable when (wall == 2 && net == 0) {
                wall = 1;
                cause = 0;
                tries = 0;
                ups = 0;
                quick = 0;
                dacts = 0;
                dticks = 0;
            }
            action WorkedThenCutOff when (wall == 2) {
                wall = 1;
                cause = 1;
                tries = 0;
                ups = 0;
                quick = 0;
                dacts = 0;
                dticks = 0;
            }
            // Taken: the worker works on, the wall gone — the episode over.
            action Taken when (wall == 2 && net == 1) {
                wall = 0;
                cause = 0;
                tries = 0;
                ups = 0;
                quick = 0;
                dacts = 0;
                dticks = 0;
            }

            invariant AMeasuredOutageIsTypedIntoAtMostOnceAHold:
                dacts == 0 || (dacts == 1 && Hold <= dticks) || Hold + Hold <= dticks;
            invariant AtMostTwoActsAtOncePerEpisode: quick <= AtOnce;
        }
    }
}

/// CODEX'S RATE-LIMIT NUDGE AND THE SAVE-THEN-WAIT SWITCH (aterm-agent
/// `supervise::policy::approval::rate_nudge` and `turn_end`'s `WindDown`;
/// the owner, 2026-09-28: the supervisor "should only use gpt-6-luna switch
/// like that for codex to git commit and push work and then wait for the
/// original model settings, not continue work in a sandbox", only near a
/// real limit, ≥ 90%).
///
/// The world: Codex's goal starts its own turns (`GoalTurn`) until it ends;
/// a turn run near the limit on the thread's own model arms the nudge
/// (`Pend`), shown at the next turn's end (`TurnEnds`) — a nudge armed
/// before a switch can show at the wind-down's end, stale; the usage window
/// reads far (`Far`, a reset or the owner's) and near again; the thread
/// falls into a sandbox its launch bypassed (`SandboxFalls`, the Codex
/// daemon's self-restart of 2026-09-28 06:05Z); a person sends a message
/// (`PersonTurn` — while the switch is open, only between the goal's
/// turns), or types `/goal resume` during the hold (`PersonResumes`: the
/// goal's turn is then theirs), which releases the switch where its turn
/// ENDS — the real policy judges a turn at its end, and behind the nudge's
/// box only once the box is answered (with the switch still open, so the
/// box gets its keep); and Codex's goal RESUMES BY ITSELF after the harness
/// stopped it (`GoalEscapes`): the stop that does not hold.
///
/// The approval loop answers the nudge: its switch only near the limit,
/// with no switch open, the thread on its own model (`PressSwitch`: the
/// goal turn Codex runs under the box keeps running — it is Codex's own,
/// `run`), else its keep (`PressKeep`). The harness STOPS Codex's goal while
/// the switch holds the session (`StopGoal`: its Esc on a running goal turn,
/// or `/goal pause` at a free point outside a sandbox; never a person's
/// turn, never under a box), at most `Stops` times a switch, then tells a
/// person, once (`Tell`). The turn-end policy: a continuation only with no
/// switch open, no goal pursued and no sandbox (`Continue`); the wind-down
/// once, outside a sandbox (`TypeWindDown`); the thread's own model and
/// effort back after it (`Restore`, outside a sandbox, the goal stopped
/// first), then held; at the reset, the goal stopped, the goal resumed or
/// the session carried on (`Resume`), never into a sandbox. `ph`: 0 free, 1
/// wind-down owed, 2 winding down, 3 restore owed then the hold, 4 holding,
/// 5 restore owed then free (a person took over).
///
/// THE ONE ASSUMPTION the abstraction makes, stated: a turn Codex runs by
/// itself while the switch is open (`run`) does not END before the harness
/// stops it (`TurnEnds` waits for `StopGoal`) unless the stops are spent and
/// a person told — the loop reads the running turn at every busy read,
/// seconds apart, and a goal turn is minutes of work. So a turn stopped at
/// its first busy read is no work, and a goal that escapes every stop runs
/// on only TOLD. And a turn ENDS where its work ends: a Codex background
/// terminal left running under an ended turn (which the screen reads busy
/// for as long as it runs) is that turn's end for the switch's steps — the
/// loop takes such a break as the switch's point, however the turn ended —
/// idle, on a question, on a wall (`TurnEnds` is fair only because it does).
/// `person` is a person's hand ANYWHERE in the running turn, as the loop
/// reads it at a busy read (a keystroke since the switch opened within the
/// turn's own work, latched for the turn; one within the grace holds the
/// stop only while the grace lasts) — not their grace alone, which ran out
/// two minutes into their own turn. `cov` is a person's hand in the turn
/// whose end the nudge's box COVERED: the box marks that turn's end, so the
/// goal turn Codex starts under it is never theirs (`GoalTurn` clears
/// `person`) — the loop measures a turn from the box on, and floors a
/// person's keystrokes at the switch's opening. `over`: the save's OWN turn
/// has run past its bound (`WindDownRunsLong`, the loop's `WIND_DOWN_BOUND`
/// of busy work since the save was typed — the owner's rule that the cheaper
/// model only commits and pushes); under the same assumption as a goal turn,
/// it does not end before the harness's one Esc stops it (`StopWindDown`,
/// the marker judged at the interrupted point), unless a person's hand is in
/// it.
///
/// The invariants — each the day's defect, and each caught by its `Buggy`
/// branch: NoUntoldWorkWhileSwitched (a turn on the cheaper model or during
/// the switch that the harness typed, or that Codex ran to its end unstopped
/// and untold: the `keep going` typed 8 s after the press, ten more on luna,
/// eighty-nine goal turns; and, the round-3 re-review's regression, the goal
/// turn under the box spared because a person's hand was in the turn the
/// box covered; and, the round-4 re-review's gap, the save's own turn left
/// to run on the cheaper model past its bound, `WindDownRunsOn`),
/// NoPersonsTurnStopped (the busy-read stop's Esc
/// into a person's own message or `/goal resume` once their grace ran out,
/// the re-review of 2026-09-28), OneWindDownPerSwitch (the stale nudge at the
/// wind-down's end taken for a new switch), HeldOnTheOriginal (the hold on
/// the cheaper model, D7's shape transplanted), SwitchedOnlyNearTheLimit
/// (the 03:30Z press at 1%), NothingTypedIntoASandbox (luna's eighty turns
/// in the `:workspace` sandbox, the save instruction among what may not go
/// there), NoContinuationIntoAPursuedGoal (all eleven continuations landed
/// inside a running goal turn). The liveness claim
/// ([`supervisor_codex_rate_nudge_liveness`]) is that the session comes back
/// to its own model, unless a person was told why it cannot. Tier-1
/// (aterm-agent `tests/supervise_conformance_codex_rate_nudge.rs`) walks
/// every reachable state with the REAL approval decider, `decide_turn_end`,
/// `TurnEndState` and its goal stop in tow; no `#[refines]` anchors
/// (aterm-spec is a dev-dependency of aterm-agent).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_codex_rate_nudge_model() -> Model {
    crate::ty_model! {
        SupervisorCodexRateNudge {
            const Buggy = 0;
            const Stops = 3;
            var model = 0;
            var goal = 1;
            var paused = 0;
            var turn = 0;
            var pend = 0;
            var shown = 0;
            var near = 1;
            var sandbox = 0;
            var ph = 0;
            var wd = 0;
            var work = 0;
            var farsw = 0;
            var sandwork = 0;
            var steer = 0;
            var person = 0;
            var run = 0;
            var stops = 0;
            var told = 0;
            var pstop = 0;
            var cov = 0;
            var over = 0;

            // ---- Codex and the world ----
            action GoalTurn when (goal == 1 && turn == 0) {
                turn = 1;
                person = 0;
                run = if ph == 1 || ph == 3 || ph == 4 { 1 } else { 0 };
                work = if ph == 0 && model == 1 { 1 } else { work };
            }
            action GoalEnds when (goal == 1 && turn == 0 && ph == 0) {
                goal = 0;
            }
            // The goal the harness stopped resumes by itself: the stop did
            // not hold.
            action GoalEscapes when (
                goal == 0 && paused == 1 && turn == 0 && (ph == 1 || ph == 3 || ph == 4)
            ) {
                goal = 1;
            }
            action Pend when (pend == 0 && near == 1 && model == 0 && turn == 1) {
                pend = 1;
            }
            // A turn's end: a person's releases the switch there (the real
            // policy judges a turn at its end) — unless the nudge covers the
            // end, and then once the box is answered. A turn Codex runs by
            // itself while switched ends only once the harness has given up
            // stopping it, and the save's own turn past its bound only once
            // the harness stopped it (the assumption above).
            action TurnEnds when (
                turn == 1 && (run == 0 || told == 1) && (over == 0 || person == 1)
            ) {
                turn = 0;
                run = 0;
                over = 0;
                shown = if pend == 1 { 1 } else { shown };
                pend = 0;
                ph = if ph == 2 {
                    3
                } else if person == 1 && pend == 0 && ph == 4 {
                    0
                } else if person == 1 && pend == 0 && (ph == 1 || ph == 3) {
                    5
                } else {
                    ph
                };
                paused = if person == 1 && pend == 0 && (ph == 1 || ph == 3 || ph == 4) {
                    0
                } else {
                    paused
                };
                person = if pend == 0 { 0 } else { person };
                cov = if pend == 1 { person } else { 0 };
            }
            // The save's own turn runs past its bound. Only from a SOUND
            // state — no day's defect's witness set, one wind-down — which is
            // every state the committed model reaches (so `Buggy = 0` is
            // unchanged): the `Buggy = 1` space must fit the interpreter's
            // budget (the non-vacuity ratchet's buggy-space test; 98 124 of
            // 100 000 states with this guard, 124 372 without it), and `over`
            // would otherwise double every wind-down state the other defects
            // reach.
            action WindDownRunsLong when (
                ph == 2
                    && turn == 1
                    && over == 0
                    && wd == 1
                    && work == 0
                    && sandwork == 0
                    && steer == 0
                    && pstop == 0
                    && farsw == 0
            ) {
                over = 1;
            }
            action Far when (near == 1) {
                near = 0;
            }
            action NearAgain when (near == 0 && ph == 0 && model == 0 && turn == 1) {
                near = 1;
            }
            action SandboxFalls when (sandbox == 0) {
                sandbox = 1;
            }
            action PersonTurn when (turn == 0 && shown == 0 && (goal == 0 || ph == 0)) {
                turn = 1;
                person = 1;
            }
            // A person's `/goal resume` during the hold: the goal's turn it
            // starts is theirs.
            action PersonResumes when (
                ph == 4 && goal == 0 && paused == 1 && turn == 0 && shown == 0
            ) {
                goal = 1;
                turn = 1;
                person = 1;
                run = 0;
            }

            // ---- the approval loop, at the nudge ----
            action PressSwitch when (shown == 1 && ph == 0 && near == 1 && model == 0) {
                shown = 0;
                model = 1;
                ph = 1;
                wd = 0;
                person = 0;
                stops = 0;
                told = 0;
                run = turn;
                cov = if turn == 0 { 0 } else { cov };
            }
            // The keep; the turn end the box covered is judged after it.
            action PressKeep when (shown == 1 && (1 <= ph || near == 0)) {
                shown = 0;
                ph = if person == 1 && turn == 0 && ph == 4 {
                    0
                } else if person == 1 && turn == 0 && (ph == 1 || ph == 3) {
                    5
                } else {
                    ph
                };
                paused = if person == 1 && turn == 0 && (ph == 1 || ph == 3 || ph == 4) {
                    0
                } else {
                    paused
                };
                person = if turn == 0 { 0 } else { person };
                cov = if turn == 0 { 0 } else { cov };
            }

            // ---- the harness stops Codex's goal while switched ----
            // Its Esc on a goal turn running by itself, or `/goal pause` at
            // a free point outside a sandbox: the goal paused, owed its
            // resume at the reset.
            action StopGoal when (
                goal == 1
                    && person == 0
                    && shown == 0
                    && stops <= Stops - 1
                    && (ph == 1 || ph == 3 || ph == 4)
                    && ((turn == 1 && run == 1) || (turn == 0 && sandbox == 0))
            ) {
                goal = 0;
                paused = 1;
                turn = 0;
                run = 0;
                stops = stops + 1;
                cov = 0;
            }
            // The save's own turn past its bound: its one Esc, the marker
            // judged at the interrupted point, the thread's own model owed
            // back (none of the goal's stops).
            action StopWindDown when (
                ph == 2 && turn == 1 && over == 1 && person == 0 && shown == 0
            ) {
                turn = 0;
                over = 0;
                ph = 3;
            }
            // The stops spent and the goal still pursued: a person is told.
            action Tell when (
                goal == 1
                    && person == 0
                    && shown == 0
                    && stops == Stops
                    && told == 0
                    && (ph == 1 || ph == 3 || ph == 4)
                    && ((turn == 1 && run == 1) || turn == 0)
            ) {
                told = 1;
            }

            // ---- the turn-end policy, at a free point ----
            action Continue when (
                ph == 0 && turn == 0 && shown == 0 && goal == 0 && sandbox == 0
            ) {
                turn = 1;
                work = if model == 0 { work } else { 1 };
            }
            action TypeWindDown when (
                ph == 1 && goal == 0 && turn == 0 && shown == 0 && sandbox == 0
            ) {
                ph = 2;
                turn = 1;
                wd = if wd <= 1 { wd + 1 } else { wd };
            }
            action Restore when (
                (ph == 3 || ph == 5)
                    && turn == 0
                    && shown == 0
                    && sandbox == 0
                    && (ph == 5 || goal == 0)
            ) {
                model = 0;
                ph = if ph == 3 { 4 } else { 0 };
            }
            action Resume when (
                ph == 4
                    && near == 0
                    && turn == 0
                    && shown == 0
                    && model == 0
                    && sandbox == 0
                    && goal == 0
            ) {
                ph = 0;
                goal = if paused == 1 { 1 } else { goal };
                turn = if paused == 1 { turn } else { 1 };
                paused = 0;
            }

            // ---- Buggy = 1: the day's defects ----
            // `keep going` typed 8 s after the press, and ten more on luna.
            action ContinueWhileSwitched when (
                Buggy == 1 && (ph == 1 || ph == 3 || ph == 4) && turn == 0 && shown == 0
            ) {
                turn = 1;
                work = 1;
            }
            // The switch pressed and Codex's goal left to run on: its turn
            // ends on the cheaper model, nobody stopping it or told.
            action SwitchKeepsGoal when (Buggy == 1 && run == 1 && turn == 1 && told == 0) {
                turn = 0;
                run = 0;
                work = 1;
            }
            // The stale nudge at the wind-down's end taken for a new switch.
            action NudgeRewinds when (Buggy == 1 && shown == 1 && 3 <= ph && ph <= 4) {
                shown = 0;
                model = 1;
                ph = 1;
            }
            // Held on the cheaper model, restored only at the reset.
            action HoldBeforeRestore when (Buggy == 1 && ph == 3 && turn == 0 && shown == 0) {
                ph = 4;
            }
            // Carried on on the cheaper model at the reset.
            action ResumeOnCheap when (
                Buggy == 1 && ph == 3 && near == 0 && turn == 0 && shown == 0
            ) {
                ph = 0;
                turn = 1;
                work = 1;
            }
            // A loop restart that reads no switch back.
            action ForgetOnRestart when (Buggy == 1 && (ph == 1 || ph == 3 || ph == 5)) {
                ph = 0;
                paused = 0;
            }
            // The 03:30Z press: the nudge switched at 1%, a stale nudge.
            action PressAtFar when (
                Buggy == 1 && shown == 1 && ph == 0 && near == 0 && model == 0
            ) {
                shown = 0;
                model = 1;
                ph = 1;
                wd = 0;
                farsw = 1;
                person = 0;
                stops = 0;
                told = 0;
                run = turn;
            }
            // A continuation typed into a thread fallen into a sandbox.
            action ContinueInSandbox when (
                Buggy == 1 && sandbox == 1 && ph == 0 && turn == 0 && shown == 0
            ) {
                turn = 1;
                sandwork = 1;
            }
            // The save instruction typed into a thread fallen into a sandbox.
            action WindDownInSandbox when (
                Buggy == 1 && sandbox == 1 && ph == 1 && goal == 0 && turn == 0 && shown == 0
            ) {
                ph = 2;
                turn = 1;
                wd = if wd <= 1 { wd + 1 } else { wd };
                sandwork = 1;
            }
            // A continuation typed while Codex's goal runs its own turns.
            action ContinueUnderGoal when (
                Buggy == 1 && goal == 1 && ph == 0 && turn == 0 && shown == 0
            ) {
                turn = 1;
                steer = 1;
            }
            // The busy-read stop's Esc into a PERSON'S turn — their message
            // while the switch is open, their `/goal resume` during the hold
            // — once their grace ran out mid-turn.
            action StopPersonsTurn when (
                Buggy == 1
                    && person == 1
                    && turn == 1
                    && shown == 0
                    && (ph == 1 || ph == 3 || ph == 4)
            ) {
                paused = if goal == 1 { 1 } else { paused };
                goal = 0;
                turn = 0;
                person = 0;
                pstop = 1;
            }
            // The round-3 re-review's regression: the loop's span carried
            // across the nudge's box, so a person's keystroke in the turn it
            // covered read as their hand in the goal turn Codex started under
            // it on the cheaper model — never stopped, never told.
            action CoveredHandSparesGoal when (
                Buggy == 1 && cov == 1 && run == 1 && turn == 1 && shown == 0 && told == 0
            ) {
                turn = 0;
                run = 0;
                cov = 0;
                work = 1;
            }

            // The round-4 re-review's gap: the save's own turn runs on the
            // cheaper model past its bound, nothing stopping it — its end
            // judged only whenever the cheaper model chooses to stop.
            action WindDownRunsOn when (Buggy == 1 && ph == 2 && turn == 1 && over == 1) {
                turn = 0;
                over = 0;
                ph = 3;
                work = 1;
            }

            invariant NoUntoldWorkWhileSwitched: work == 0;
            invariant NoPersonsTurnStopped: pstop == 0;
            invariant OneWindDownPerSwitch: wd <= 1;
            invariant HeldOnTheOriginal: ph <= 3 || 5 <= ph || model == 0;
            invariant SwitchedOnlyNearTheLimit: farsw == 0;
            invariant NothingTypedIntoASandbox: sandwork == 0;
            invariant NoContinuationIntoAPursuedGoal: steer == 0;
        }
    }
}

/// THE SESSION COMES BACK to its own model and effort
/// ([`supervisor_codex_rate_nudge_model`]): `[]<>((model == 0 && (ph == 0 ||
/// ph == 4)) || told == 1 || sandbox == 1)` — free, or held on its own model
/// — whatever the nudge, the person and the window do; unless a person was
/// told it cannot (Codex's goal escaped every stop), or the thread fell into
/// a sandbox (nothing is typed into it; the loop says how to fix it).
/// Fairness, each load-bearing: WF(TurnEnds) — a turn ends (the loop's act
/// deadline escalates one that does not; one whose work ended under a Codex
/// background terminal ends there for the switch's steps, the loop's
/// background break); WF(StopWindDown) — the save's turn past its bound is
/// stopped (it does not end by itself before); WF(TypeWindDown) — the owed
/// wind-down is typed; WF(PressKeep) — a stale nudge at the wind-down's end
/// is kept, else it blocks the restore for good; WF(StopGoal) — a goal turn
/// running while switched is stopped (it does not end by itself before);
/// WF(Tell) — the stops spent, a person is told; SF(Restore) — a person
/// typing turn after turn disables the restore between their turns, so weak
/// fairness would not get it made. Mutants, each breaking it alone:
/// `ResumeOnCheap` and `ForgetOnRestart` leave the session free on the
/// cheaper model with nothing to restore it (D7's continuation, and the
/// ledger the incident's press never wrote); `HoldBeforeRestore` holds it
/// there.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_codex_rate_nudge_liveness() -> crate::derive::Liveness {
    use crate::derive::{and_, eq, int, or_, var};
    crate::derive::Liveness {
        name: "TheSessionComesBack",
        goal: or_(
            and_(
                eq(var("model"), int(0)),
                or_(eq(var("ph"), int(0)), eq(var("ph"), int(4))),
            ),
            or_(eq(var("told"), int(1)), eq(var("sandbox"), int(1))),
        ),
        weak: vec![
            "TurnEnds",
            "TypeWindDown",
            "PressKeep",
            "StopGoal",
            "Tell",
            "StopWindDown",
        ],
        strong: vec!["Restore"],
        mutants: vec!["ResumeOnCheap", "ForgetOnRestart", "HoldBeforeRestore"],
    }
}
