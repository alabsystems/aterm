// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's own bounded machines (aterm-agent `supervise`, lane B2):
//! the session claim it acts under, the focus-then-Enter choice it makes on
//! an unnumbered dialog, the turn-end policy, the keys it sends to answer
//! Claude Code's question dialog, the keystrokes of a decline, and when it
//! types into an API error the network caused.

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
