// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's own bounded machines (aterm-agent `supervise`, lane B2):
//! the session claim it acts under, the focus-then-Enter choice it makes on
//! an unnumbered dialog, the turn-end policy under Cruise and Land, the keys it
//! sends to answer Claude Code's question dialog, the keystrokes of a
//! decline, when it types into an API error the network caused, the
//! stall's remedy, and the one ask a box gets however it is redrawn.

use aterm_spec::derive::{Liveness, Model};

/// The supervisor claim (aterm-agent `supervise/claim.rs`): a loop presses only while its
/// last word from the server was that the claim is its own (`view` 1), and renews it before
/// any request once `Renew` ticks have passed, so a lease that lapsed (`Ttl` ticks without
/// a request) and was taken by another supervisor is found out by the renewal before the
/// next press.
///
/// `held` is the server's claim (0 none, 1 this loop's, 2 another's); `view` the loop's
/// last reading (1 its own, 2 another's, 3 pending: its last request was not served, so it
/// asks again and presses nothing until answered); `age` the ticks since it last asked.
/// `Buggy = 1` presses without the fresh renewal and without its fence, and is caught.
/// Tier-1 (aterm-agent `supervise/run_engine_tests.rs`) drives the real `Session` over the
/// scripted server as an `External` loop. A host with no claim (`CLAIM unavailable`) is
/// outside this model: the loop acts without one.
///
/// The proven self-claim (native harness contract §2): the server lets a proven self-claim
/// replace the window host's held claim (`ProvenReplaces`, `held` 1 → 2) with no renewal
/// first, while the loop still reads it as its own (`view` 1). Only the hosted loop's claim
/// is replaced so, and it fences every write with its own holder (`if-supervisor=<holder>`,
/// `fenced` 1): the server admits a fenced write only while that holder holds the claim,
/// counted from the fence's read to the write's end, and answers a replacement only once
/// those writes have landed (aterm-gui `session_timeline::FencedWrite`), so a fenced press
/// under another's claim lands nothing. An `External` loop does not fence, is never
/// replaced so, and is held to the renewal.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_claim_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorClaim {
            const Buggy = 0;
            const Renew = 1;
            const Ttl = 3;
            var started = 0;
            var fenced = 1;
            var held = 0;
            var view = 0;
            var age = 0;
            var pressed_under_other = 0;

            // Which loop this is: the window host's (it fences) or an
            // external supervisor's (it does not).
            action Hosted when (started == 0) {
                started = 1;
            }
            action External when (started == 0) {
                started = 1;
                fenced = 0;
            }
            // Time passes with no request from the loop.
            action Tick when (started == 1 && age <= Ttl - 1) {
                age = age + 1;
            }
            // The server lets an unrenewed lease go.
            action Lapse when (started == 1 && held == 1 && age == Ttl) {
                held = 0;
            }
            action OtherClaims when (started == 1 && held == 0) {
                held = 2;
            }
            // A proven self-claim replaces the hosted loop's held claim.
            action ProvenReplaces when (started == 1 && held == 1 && fenced == 1) {
                held = 2;
            }
            action OtherReleases when (started == 1 && held == 2) {
                held = 0;
            }
            // Any request of the loop's, preceded by the renewal once it is
            // due: the server grants a free claim and refuses a held one.
            action Renewal when (started == 1 && (Renew <= age || view == 3)) {
                view = if held == 2 { 2 } else { 1 };
                held = if held == 2 { 2 } else { 1 };
                age = 0;
            }
            // A claim request the server did not serve (an outage): the
            // loop cannot tell whose the session is.
            action Unserved when (started == 1 && (Renew <= age || view == 3)) {
                view = 3;
                age = 0;
            }
            // A press lands under another's claim only unfenced: a fenced
            // one is refused by the server there and lands nothing.
            action Press when (
                started == 1 && view == 1 && (age <= Renew - 1 || Buggy == 1)
            ) {
                pressed_under_other = if held == 2 && (fenced == 0 || Buggy == 1) {
                    1
                } else {
                    pressed_under_other
                };
            }

            invariant NoPressUnderAnothersClaim: pressed_under_other == 0;
        }
    }
}

/// The choice on an unnumbered dialog (aterm-agent `supervise/press.rs`, `press_focused`;
/// the folder-trust dialog): the focus is moved, the screen read again, and Enter is pressed
/// only when that read shows the focus on the chosen option and the screen is still the one
/// it saw (the generation fence), so an Enter never lands on `No, exit`.
///
/// `focus` is the dialog's cursor (0 `No, exit`, 1 the chosen option); `confirmed` the last
/// read showed it on the chosen one; `fresh` the screen has not moved since that read. A
/// move may land or be lost; the screen may move (and the focus with it) at any time. A move
/// the dialog did not take is made again, at most `Tries` times (a dialog drawn before its
/// input is live drops the key; the loop's `MAX_CHANGED_PRESSES` less the first move).
/// `Buggy = 1` presses Enter on the move alone, unread, and is caught.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_focus_choice_model() -> Model {
    aterm_spec::ty_model! {
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
/// `decide_turn_end`), FULLY AUTOMATIC, UNDER AUTOPILOT CRUISE: what the
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
/// (D1 of the live E2E, `docs/AUDIT-claude-harness-2026-09-23.md`): the
/// harness's OWN typed turns (its
/// upgrade notice, its carry-on, and what the supervisor itself typed, by
/// its ledger) are no task, however much the agent works on them.
///
/// Autopilot Cruise (the owner, 2026-10-19): nothing the worker says ends the
/// task. A continuation's reply that CLAIMS DONE (`WorkedClaimsDone`: a done
/// phrase, an idle report, the retired `ATERM-TASK-DONE` line, a `Nothing
/// left:` defense) is an ordinary yield — a short turn of the streak's, its
/// act the defend-and-find-more prompt (`continue-done-check@v1`, typed in
/// the continuation's place on the same gates; `claim` 1) — and the task
/// goes on (`CruiseNeverRests`). A reply that names what only a person can
/// give (`WorkedNeeds`: a `NEEDS YOU:` line) puts it on the person's screen
/// (the loop's attention entry, outside this pure policy) and NEVER holds
/// the point: the act goes as it would, the need acked beside it
/// (`ANeedNeverBlocks`) — a tell distinct from the escalation past the cap
/// (`NeverEscalateUnderFullPower` is untouched by it). Someone else's turn
/// answers it (`need` 0). The task's face under Cruise is open at every
/// state; its landed and theirs faces are Land's ([`supervisor_land_model`]).
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
/// worker's own turns left them (N1: the READY
/// answer and the carry-on's reply, read as someone else's short turns,
/// backed a finished stage's continuation off 4 minutes); `HarnessTurnLong`
/// the same turn answered with REAL WORK (a carry-on's answer is the
/// worker's own work, resumed): still no task, and the streak ends as after
/// any turn of real work (else a 20-minute carry-on answer is backed off
/// like a short turn). `harness` counts
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
/// (typing it is a person's keystroke: its grace runs). `other` 1 once a
/// LIVE READ of someone else's turn came before its end (`OtherTurnRuns`, no
/// act of the policy's awaited): a turn runs, nothing is typed, and its end
/// is `HumanWork`. `claim` 1 while the point's reply claims done, `need` 1
/// while a need stands; `rested` and `held` are the ghosts of the defects
/// below. `Buggy = 1` ignores
/// the box, the person, the draft and the back-off, escalates "worker
/// reports done" after two short turns, keeps awaiting an unseen point past
/// its deadline (a silent latch), reads the harness's own turn as a task (D1:
/// `keep going` and `answer_text` into a session nobody had asked anything),
/// as a short turn of the worker's (N1), and its answer of real work as no
/// end of the streak; takes the worker's done claim as the task's end and
/// types nothing more until someone else's turn (the policy before FOREVER
/// CRUISE, caught by `CruiseNeverRests`); and holds the point a need stands
/// at for the person, who may never come (caught by `ANeedNeverBlocks`) —
/// each caught. (Branches of live actions, no dead action: the `Buggy = 1`
/// space is past the interpreter's budget, which a dead action's audit
/// walks whole.) Tier-1
/// (aterm-agent `tests/supervise_conformance_turn_end.rs`) drives the real
/// decider along every reachable state of this model, with no cap and with
/// one, and checks that it types exactly where the model allows it — the
/// defend prompt where the point claims done, the continuation (or the
/// priority's answer) elsewhere — waits
/// where a wall, a person or a back-off holds it, is never silent at a free
/// point of a session with a task, and types nothing into one without —
/// its `tasked` the REAL `TaskScan`'s over the record the walk writes (a
/// person's prompt for `HumanWork`, the harness's marked one and the
/// supervisor's ledgered `keep going` for `HarnessTurn`) — and its streak
/// the real one, a `HarnessTurn` the host's typed turn
/// (`TurnEndState::host_typed`) answered short, a `HarnessTurnLong` the same
/// answered with real work; its done claim, its standing needs and its live
/// read of someone else's turn the model's `claim`, `need` and `other`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_turn_end_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorTurnEnd {
            const Buggy = 0;
            const Budget = 0;
            const Take = 2;
            const Streak = 3;
            // A cap the owner wrote: its window and the escalation past it
            // exist only there.
            scenario Budget = 2;
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
            var claim = 0;
            var other = 0;
            var need = 0;
            var rested = 0;
            var held = 0;

            action BoxAppears when (box_up == 0 && pending == 0 && other == 0) {
                box_up = 1;
            }
            action BoxLeaves when (box_up == 1) {
                box_up = 0;
            }
            // (Each clock runs alone — a wall's wait, a back-off, the cap's
            // window — so the real side can move its clock by exactly the
            // one that runs out.) A reply a wall cut claims nothing.
            action WallAppears when (
                wall == 0 && box_up == 0 && pending == 0 && backoff == 0 && other == 0
            ) {
                wall = 1;
                claim = 0;
            }
            action WallDue when (wall == 1) {
                wall = 2;
            }
            action PersonTypes when (person == 0 && pending <= 2) {
                person = 1;
            }
            // Text left in the composer: typed by a person, whose grace runs.
            action DraftLeft when (draft == 0 && pending == 0 && box_up == 0 && other == 0) {
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
            // answered), and the person has answered any need that stood.
            action HumanWork when (pending == 0 && box_up == 0 && wall == 0) {
                short = 0;
                backoff = 0;
                earned = 0;
                long = 1;
                draft = 0;
                asked = 1;
                tasked = 1;
                claim = 0;
                other = 0;
                need = 0;
                rested = 0;
                held = 0;
            }
            // A LIVE READ OF SOMEONE ELSE'S TURN (a person's, an
            // orchestrator's: no act of the policy's awaited), before its
            // end: nothing is typed while it runs.
            action OtherTurnRuns when (
                pending == 0 && box_up == 0 && wall == 0 && draft == 0 && other == 0
            ) {
                other = 1;
            }
            // The harness's own turn (its notice or carry-on, the upgrade's two)
            // answered short: no task, and no short turn of the worker's.
            action HarnessTurn when (
                harness <= 1 && pending == 0 && box_up == 0 && wall == 0 && draft == 0 &&
                other == 0
            ) {
                harness = harness + 1;
                short = if Buggy == 1 && short <= Streak - 1 { short + 1 } else { short };
                backoff = if Buggy == 1 { 1 } else { backoff };
                tasked = if Buggy == 1 { 1 } else { tasked };
            }
            // …answered with real work (a carry-on's answer is the worker's own
            // work, resumed): still no task, and the streak ends.
            action HarnessTurnLong when (
                harness <= 1 && pending == 0 && box_up == 0 && wall == 0 && draft == 0 &&
                other == 0
            ) {
                harness = harness + 1;
                short = if Buggy == 1 { short } else { 0 };
                backoff = if Buggy == 1 { backoff } else { 0 };
                earned = if Buggy == 1 { earned } else { 0 };
                long = 1;
                tasked = if Buggy == 1 { 1 } else { tasked };
                claim = 0;
            }
            // The continuation — or, where the point claims done, the
            // defend-and-find-more prompt in its place: one act of the
            // policy's, on the same gates. A standing need holds nothing.
            action Continue when (
                pending == 0 && wall == 0 && other == 0 && rested == 0 && held == 0 &&
                (tasked == 1 || Buggy == 1) &&
                ((box_up == 0 && person == 0 && draft == 0 && backoff == 0) || Buggy == 1) &&
                (Budget == 0 || used <= Budget - 1 || (Buggy == 2 && used <= Budget))
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
                claim = 0;
            }
            action WorkedLong when (pending == 1) {
                pending = 0;
                short = 0;
                backoff = 0;
                earned = 0;
                long = 1;
                claim = 0;
            }
            // The reply CLAIMS DONE (a done phrase, the retired token, a
            // `Nothing left:` defense): it ends nothing — the defend prompt
            // is owed at this point, past the back-off a done claim earns
            // (a short turn of the streak's, whatever it worked). `Buggy`:
            // the task ends on it, nothing more typed until someone else's
            // turn (the policy before Autopilot Cruise).
            action WorkedClaimsDone when (pending == 1) {
                pending = 0;
                short = if short <= Streak - 1 { short + 1 } else { short };
                backoff = 1;
                earned = 1;
                long = 0;
                claim = 1;
                rested = if Buggy == 1 { 1 } else { rested };
            }
            // The reply did real work and names what only a person can give
            // (a `NEEDS YOU:` line): the need stands — on the person's
            // screen — and the point is free as after any real work
            // (`Buggy`: held for the person until someone else's turn).
            action WorkedNeeds when (pending == 1) {
                pending = 0;
                short = 0;
                backoff = 0;
                earned = 0;
                long = 1;
                claim = 0;
                need = 1;
                held = if Buggy == 1 { 1 } else { held };
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
                pending == 0 && wall == 2 && other == 0 && rested == 0 &&
                (tasked == 1 || Buggy == 1) &&
                ((box_up == 0 && person == 0 && draft == 0) || Buggy == 1) &&
                (Budget == 0 || used <= Budget - 1)
            ) {
                typed_taskless = if asked == 0 { 1 } else { typed_taskless };
                typed_blocked = if box_up == 1 || person == 1 { 1 } else { typed_blocked };
                typed_over = if draft == 1 { 1 } else { typed_over };
                used = if Budget == 0 { used } else { used + 1 };
                pending = 2;
            }
            // The draft sent as it stands, in the act's place: a continuation's
            // turn, or a retry at a wall whose wait is over.
            action SubmitDraft when (
                pending == 0 && draft == 1 && (wall == 0 || wall == 2) && tasked == 1 &&
                other == 0 && rested == 0 && held == 0 &&
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
                pending == 0 && box_up == 0 && person == 0 && tasked == 1 &&
                other == 0 && rested == 0 && (
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
            // Dial 2 checks the cap one turn late (it bites at the `Budget = 2` scenario).
            invariant WithinTheCap: Budget == 0 || used <= Budget;
            invariant NoSilentLatch: latched == 0;
            invariant NeverTypeIntoATasklessSession: typed_taskless == 0;
            invariant NeverBackOffForTheHarness: backoff <= earned;
            invariant NeverBackOffAfterRealWork: backoff == 0 || long == 0;
            invariant CruiseNeverRests: rested == 0;
            invariant ANeedNeverBlocks: held == 0;
        }
    }
}

/// Autopilot Cruise (aterm-agent `supervise/policy/turn_end.rs`, the owner's
/// direction of 2026-10-19: *"Cruise is always looking for more completeness
/// and perfection"*). NOTHING ENDS A CRUISE TASK: no done phrase, no retired
/// `ATERM-TASK-DONE` line, no `Nothing left:` defense, no reply to the
/// defend prompt, no task record and no old journal's `DONE` note. A reply
/// that CLAIMS DONE is owed the defend-and-find-more prompt
/// (`continue-done-check@v1`, `Check`): land every open item, look harder,
/// defend that nothing is left — and keep looking. Its suffixes name the
/// open work the claim's own words name (`open` 1: `Still open:`, `not run`,
/// …) or the work the session's repository holds that no remote main has
/// (`open` 2: the host's `git` probe — uncommitted, untracked, stashed,
/// unpushed or not on the remote's default branch); the claim is defended
/// either way, with or without that work (`CruiseNeverRests` holds over
/// both).
///
/// `claim` 1 the point's reply claims done (the defend prompt owed);
/// `pending` 0 at a point, 1 a continuation's turn running, 2 the defend
/// prompt's, 3 a wall's act's (the 529's retry, the limit's resume,
/// `/compact`), 4 an answer's; `walled` the point shows a wall — its ladder
/// acts there, never the continuation or the defend prompt (a reply a wall
/// cut leaves the claim before it standing); `asked` the point asks a
/// question (an act's short yield that asks, `YieldAsk`), answered, never
/// continued.
/// THE BACK-OFF (cost control, never a stop): `short` is the short streak
/// (4 standing for any streak past `MANAGED_SHORT_TURNS`, 3): a
/// continuation's, the defend prompt's or an answer's yield that claims done
/// or asks lengthens it, a turn of real work ends it, and a wall's act leaves
/// it as it was. At every new point with a streak the back-off runs
/// (`backoff` 1) until the wait the decider asked for runs out (`BackoffDue`
/// — only where it waits, no act owed at once). A session nobody waits on
/// waits it out before every act, saving an idle person's tokens; A WORKER
/// (`managed` 1, `Tasked`: the loop read its task record,
/// `TurnEndState::set_managed`) has a manager waiting on its `await task`,
/// and gets its defend prompt and its answer at once for its first three
/// short turns in a row — past them, at the back-off's pace, so a worker that
/// asks after every short turn is not answered at the model's; its
/// continuation keeps the back-off. `waited` the back-off ran out over a
/// worker's owed defend prompt or answer within its bound, `rushed` one went
/// at once past it.
/// THE BUSY SCREEN: a turn that claims done while a shell or a monitor it
/// left runs on (`*ClaimBg`; `bg` 1 while it runs) is no point: no act goes
/// at the busy screen, and the claim is decided at the idle screen the shell
/// leaves when it ends (`BackgroundEnds`) — the defend prompt, as at any
/// claim.
///
/// `unchecked` a claim was answered with a plain continuation, `typed_busy`
/// an act went at the busy screen, `rested` the task was taken as ended.
/// `Buggy = 1` continues a claim like any point, holds a worker's defend
/// prompt for the back-off yet answers it at once however long it keeps
/// asking, takes the defend prompt's own done reply as the task's end (the
/// policy before Autopilot Cruise, whose done reply to the check ended the
/// job), reads the claim at the busy screen and types the defend prompt into
/// the running shell, and adds two dead actions: an old journal's `DONE …
/// rule=finished@v1` (or a task record's `done why=marker`) taken on as the
/// end (`SeedOldDone`), and the token over a running shell read as the end
/// there (`FinishOverBackground`, the retired `finished_over_background`) —
/// caught by `CruiseNeverRests` on a path of its own each.
/// Tier-1 (aterm-agent `tests/supervise_conformance_cruise.rs`) drives the
/// real `decide_turn_end` and `TurnEndState` under Cruise along every
/// reachable state and checks it types the continuation, the defend prompt
/// (its suffixes the model's `open`), the answer, the wall's act, waits, or
/// does nothing exactly where the model does, that its task is never done
/// and a recorded done is never taken on, and that its done claim, its short
/// streak, its worker flag and the work its claim left are the model's.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_cruise_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorCruise {
            const Buggy = 0;
            var claim = 0;
            var pending = 0;
            var unchecked = 0;
            var walled = 0;
            var asked = 0;
            var managed = 0;
            var short = 0;
            var backoff = 0;
            var waited = 0;
            var rushed = 0;
            var bg = 0;
            var open = 0;
            var typed_busy = 0;
            var rested = 0;

            // The loop reads the session's task record: a manager waits.
            action Tasked when (managed == 0) {
                managed = 1;
            }
            // `Buggy`: an old journal's `DONE` note, or a task record's
            // `done` from a build before Autopilot Cruise, taken on as the end.
            action SeedOldDone when (Buggy == 1 && pending == 0 && bg == 0 && rested == 0) {
                rested = 1;
            }
            // Someone else's turn of real work: progress, a done claim (one
            // that names open work, one over a repository the probe finds
            // work in), or a wall — the streak ends, or stands at a wall.
            action OtherTurnWork when (pending == 0 && bg == 0) {
                claim = 0;
                open = 0;
                walled = 0;
                asked = 0;
                short = 0;
                backoff = 0;
            }
            action OtherTurnClaim when (pending == 0 && bg == 0) {
                claim = 1;
                open = 0;
                walled = 0;
                asked = 0;
                short = 0;
                backoff = 0;
            }
            action OtherTurnOpenClaim when (pending == 0 && bg == 0) {
                claim = 1;
                open = 1;
                walled = 0;
                asked = 0;
                short = 0;
                backoff = 0;
            }
            action OtherTurnDirtyClaim when (pending == 0 && bg == 0) {
                claim = 1;
                open = 2;
                walled = 0;
                asked = 0;
                short = 0;
                backoff = 0;
            }
            action OtherTurnWall when (pending == 0 && bg == 0) {
                claim = 0;
                open = 0;
                walled = 1;
                asked = 0;
                backoff = if short > 0 { 1 } else { 0 };
            }
            // …a claim while a shell it left runs on: the busy screen.
            action OtherTurnClaimBg when (pending == 0 && bg == 0) {
                claim = 1;
                open = 0;
                walled = 0;
                asked = 0;
                short = 0;
                backoff = 0;
                bg = 1;
            }
            // The shell ends: the idle screen it leaves is the claim's point.
            action BackgroundEnds when (bg == 1) {
                bg = 0;
            }
            // `Buggy`: the retired token read at the busy screen as the end.
            action FinishOverBackground when (Buggy == 1 && bg == 1 && rested == 0) {
                rested = 1;
            }
            // The policy's act at a free point (never at the busy screen):
            // the wall's act at a wall; the continuation where nothing was
            // claimed and nothing asked, past its back-off; the defend prompt
            // where a claim was and the answer where the point asks — a
            // worker's at once within its bound, anyone else's past the
            // back-off.
            action WallAct when (pending == 0 && bg == 0 && walled == 1 && rested == 0) {
                walled = 0;
                open = 0;
                pending = 3;
            }
            action Continue when (
                pending == 0 && bg == 0 && walled == 0 && asked == 0 && rested == 0 &&
                (backoff == 0 || Buggy == 1) && (claim == 0 || Buggy == 1)
            ) {
                unchecked = if claim == 1 { 1 } else { unchecked };
                open = 0;
                pending = 1;
            }
            action Check when (
                pending == 0 && (bg == 0 || Buggy == 1) && walled == 0 && claim == 1 &&
                rested == 0 &&
                (backoff == 0 || (managed == 1 && short <= 3 && Buggy == 0))
            ) {
                rushed = if backoff == 1 && short > 3 { 1 } else { rushed };
                typed_busy = if bg == 1 { 1 } else { typed_busy };
                open = 0;
                pending = 2;
            }
            action Answer when (
                pending == 0 && bg == 0 && walled == 0 && asked == 1 && claim == 0 &&
                rested == 0 &&
                (backoff == 0 || (managed == 1 && (short <= 3 || Buggy == 1)))
            ) {
                rushed = if backoff == 1 && short > 3 { 1 } else { rushed };
                asked = 0;
                pending = 4;
            }
            // The decider's wait for the back-off runs out — where it waits:
            // a continuation owed, or a defend prompt or an answer that does
            // not go at once.
            action BackoffDue when (
                pending == 0 && bg == 0 && walled == 0 && backoff == 1 && (
                    (claim == 0 && asked == 0) ||
                    (claim == 1 && (managed == 0 || short > 3 || Buggy == 1)) ||
                    (asked == 1 && (managed == 0 || (short > 3 && Buggy == 0)))
                )
            ) {
                waited = if managed == 1 && short <= 3 && (claim == 1 || asked == 1) {
                    1
                } else {
                    waited
                };
                backoff = 0;
            }
            // An act's yield of real work ends the streak (a wall's act's
            // leaves it, and the back-off it runs).
            action YieldWork when (pending > 0 && bg == 0) {
                pending = 0;
                claim = 0;
                open = 0;
                short = if pending == 3 { short } else { 0 };
                backoff = if pending == 3 && short > 0 { 1 } else { 0 };
            }
            // A yield that claims done — the defend prompt's own defense
            // among them — is a claim like any: short, as a yield that asks
            // is, and owed the defend prompt again (`Buggy`: the defend
            // prompt's done reply ends the task).
            action YieldClaim when (pending > 0 && bg == 0) {
                rested = if Buggy == 1 && pending == 2 { 1 } else { rested };
                claim = 1;
                open = 0;
                short = if pending == 3 { short } else if short > 3 { 4 } else { short + 1 };
                backoff = if pending == 3 && short == 0 { 0 } else { 1 };
                pending = 0;
            }
            action YieldOpenClaim when (pending > 0 && bg == 0) {
                claim = 1;
                open = 1;
                short = if pending == 3 { short } else if short > 3 { 4 } else { short + 1 };
                backoff = if pending == 3 && short == 0 { 0 } else { 1 };
                pending = 0;
            }
            action YieldDirtyClaim when (pending > 0 && bg == 0) {
                claim = 1;
                open = 2;
                short = if pending == 3 { short } else if short > 3 { 4 } else { short + 1 };
                backoff = if pending == 3 && short == 0 { 0 } else { 1 };
                pending = 0;
            }
            action YieldAsk when (pending > 0 && bg == 0) {
                claim = 0;
                asked = 1;
                open = 0;
                short = if pending == 3 { short } else if short > 3 { 4 } else { short + 1 };
                backoff = if pending == 3 && short == 0 { 0 } else { 1 };
                pending = 0;
            }
            // A yield that ended on a wall: the claim before it stands.
            action YieldWall when (pending > 0 && bg == 0) {
                pending = 0;
                walled = 1;
                asked = 0;
                open = 0;
                short = if pending == 3 { short } else { 0 };
                backoff = if pending == 3 && short > 0 { 1 } else { 0 };
            }
            // A yield that claims done while a shell it left runs on.
            action YieldClaimBg when (pending > 0 && bg == 0) {
                claim = 1;
                open = 0;
                asked = 0;
                short = if pending == 3 { short } else if short > 3 { 4 } else { short + 1 };
                backoff = if pending == 3 && short == 0 { 0 } else { 1 };
                pending = 0;
                bg = 1;
            }

            invariant CruiseNeverRests: rested == 0;
            invariant AClaimIsDefended: unchecked == 0;
            invariant AManagerIsNeverKeptWaiting: waited == 0;
            invariant AWorkerThatKeepsAskingWaitsTheBackOff: rushed == 0;
            invariant TheBusyScreenGetsNoAct: typed_busy == 0;
        }
    }
}

/// Autopilot Land (aterm-agent `supervise/policy/turn_end.rs`, `land_policy`;
/// the owner's direction of 2026-10-19: *"if landing … LAND EVERYTHING INTO
/// … REMOTE MAIN NO EXCEPTIONS"*, *"if you need something from the user, be
/// LOUDER ABOUT IT"*, *"but NEVER let that block"*). Under Autopilot Land the
/// episode's first free point gets THE LAND INSTRUCTION (`land@v1`,
/// `Instruction`), and every free point after it A NUDGE (`land-nudge@v1`,
/// `Nudge`) — with NO BOUND, paced only by the short streak's back-off — or,
/// where the point asks, THE LAND ANSWER (`land-answer@v1`, `Answer`). The
/// task is LANDED — done, nothing typed until someone else's turn — only at
/// the yield of a land act (one typed once the instruction was, or the
/// wall's retry that delivers it again, `cut`) whose reply is A CLEAN
/// DEFENSE: a `Nothing left:` line, no `NEEDS YOU:` line beside it, no open
/// work in Land's strict reading of it, AND a repository the host's probe
/// finds nothing in that no remote main has (`YieldDefense`). A defense over
/// such a repository (`YieldDefenseDirty`), one that names a need
/// (`YieldDefenseNeeds`) or open work (`YieldOpenDefense`), the retired
/// token or a done phrase (`YieldClaim`), and any defense outside a land
/// act's yield (`OtherTurnDefense`) land nothing: the next free point is
/// nudged. A need never holds an act: it is on the person's screen (the
/// loop's attention entry), and Land carries on. A landed task's record a
/// new loop reads is taken on (`SeedLanded`, `TurnEndState::seed_task_done`)
/// — never at a wall, whose act is owed.
///
/// `typed` 1 once the episode's instruction was typed; `pending` 0 at a
/// point, 1 a land act's turn (an answer after the instruction among them),
/// 2 the land answer's typed before any instruction (no land act: its
/// defense lands nothing), 3 a wall's act's; `said` what the point's reply
/// says — 0 work, 1 a done claim that is no defense (the token, a done
/// phrase), 2 a defense's words, 3 a defense naming open work; `need` 1
/// while a need stands (a defense replaces the set by its own lines;
/// someone else's turn answers it); `dirty` the host's probe finds work no
/// remote main has at the point's claim; `walled` a wall stands, `cut` it
/// cut a land act (its retry delivers it again); `asked` the point asks;
/// `short` 1 while the short streak runs (a land act's yield that claims or
/// asks lengthens it, real work ends it, a wall's act leaves it), `backoff`
/// 1 while its wait runs. `landed` 1 the task is landed; `left` 1 it is A
/// PERSON'S OWN EXCHANGE (their quick message on a task the policy was not
/// carrying — after a claim or a landing — answered: theirs, nothing typed
/// until the next turn ends).
///
/// THE TASK'S FACE (the Autopilot design's §9.4.2), as the loop tells its
/// host (`IdleHost::task_face`): `other` 1 once a LIVE READ of someone else's
/// turn came before its end (`OtherTurnRuns`); `told` the face the host was
/// last told — 0 open, 1 landed, 2 theirs — which must be the face of the
/// state at every step (`TheHostIsToldTheTasksFace`): open while someone
/// else's turn runs, else theirs, landed or open. A point tells the face it
/// moves to, and so does that live read: told only at points, `✓ landed`
/// stood through a person's new task for as long as it ran.
///
/// WHY LAND STILL HOLDS (the Autopilot design's §12.3.3), as the loop tells
/// its host (`IdleHost::land_held`, from its one teller, `Session::tell_hold`:
/// at the loop's start, at each point folded in, at each live read of
/// someone else's turn and at each act's ledger row): `hold` the hold
/// the host was last told — 0 none, 1 a hold whose legs are unread, 2 a
/// hold read at a land act's yield — and `hlegs` its legs, in the engine's
/// own bits (`HoldLeg`: NEEDS 1, REPO 2, OPEN 4, DEFENSE 8). Two ghosts say
/// what the hold is: `judged` 1 from a land act's yield that did not land
/// (`pending` 1, or a wall's retry that delivered a land act, `pending` 3
/// with `cut`) until someone else's turn (its live read or its end) or a
/// landing; `relanded` 1 from a landing (and a landed record taken on)
/// until Land's next act begins the hold again (`HoldStart::LANDED`). The
/// law `TheHostIsToldWhatHoldsTheLanding`: no hold is told before the
/// instruction, after a landing until Land's next act, at a landed task or
/// a person's own exchange; else a hold is told, read exactly where
/// `judged`, and then — unless a wall has cut the land act typed since
/// (`cut`: the legs are the yield's, the model's `dirty` the wall's) — with
/// the state's own legs: `defense` iff `said < 2`, `open` iff `said == 3`,
/// `needs` iff `said == 2 && need == 1`, `repo` iff `dirty == 1`. The
/// engine's `needs` leg is "said this turn"; the model's `need` is the
/// standing set, which `YieldWork` and `YieldClaim` keep from an earlier
/// report — so the law reads `need` only at a defense (`said == 2`), which
/// replaces the set with its own lines, and there the two agree. Not
/// modeled, and held by the engine's unit tests (3a's
/// `turn_end_hold_tests.rs`): a work report's own `NEEDS YOU:` lines, a
/// need said earlier in the turn (`turn_said`), and a reply read cut
/// (`said_cut`, the `cut` leg); the model's `cut` is a wall's cut of a land
/// act, no leg. `StaleHold = 1` leaves the hold told as it was at a yield
/// while the face is told: the face law holds, and only the hold's law
/// refutes it. `Buggy = 2` is the same mutant, the dial `aterm-models`'
/// `non_vacuity` sweep (which turns `Buggy` alone) reaches it by: one
/// mutant of the hold's own (`Buggy = 1`'s landing on a claim breaks the
/// hold's law too, as it breaks the landing's).
///
/// `quiet`, `held`, `typed_landed` and `nudges` are the defects' ghosts.
/// `Buggy = 1` lands the task on the retired token or a done phrase at a
/// land act's yield (the policy before Autopilot Land, whose done reply
/// ended it), over a repository with work no remote main has, and over a
/// defense that names a need — each caught by `LandedOnlyByCleanDefense`;
/// nudges a landed task (`NeverTypeIntoALandedTask`); takes a landed record
/// on at a wall (`AWallIsNeverLeftOnALandedTask`); tells nothing at the live
/// read (`TheHostIsToldTheTasksFace`); and adds two dead actions: the
/// landing that goes quiet after two nudges (`SpentAfterTwo`, the retired
/// `LAND_NUDGES = 2`, caught by `LandNeverGoesQuiet` and by
/// [`supervisor_land_liveness`]) and the point a standing need holds for the
/// person (`AskHolds`, caught by `ANeedNeverBlocks` and by that liveness).
/// Tier-1 (aterm-agent `tests/supervise_conformance_land.rs`) drives the real
/// `decide_turn_end` with `autopilot: Land` along every reachable state and
/// checks it types the instruction, the nudge (its suffixes the open work
/// and the repository's), the land answer and the wall's act, waits, or does
/// nothing exactly where the model does, that its landing, its person's
/// exchange, its episode, its needs and its streak are the model's, that a
/// landed record is taken on where `SeedLanded` is enabled and never at a
/// wall, and that the face its teller tells is the model's — and three
/// tellers that miss it each fail the walk: one told on the journal's
/// `DONE`/`LEFT` notes alone, one told at points alone, and one that says
/// landed for any done task. The hold its teller tells is the model's too,
/// and three tellers that miss it fail the walk: one that never tells it,
/// one told at points alone, and one never told at an act's row.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_land_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorLand {
            const Buggy = 0;
            const StaleHold = 0;
            var typed = 0;
            var pending = 0;
            var said = 0;
            var need = 0;
            var dirty = 0;
            var walled = 0;
            var cut = 0;
            var asked = 0;
            var short = 0;
            var backoff = 0;
            var landed = 0;
            var left = 0;
            var other = 0;
            var told = 0;
            var nudges = 0;
            var quiet = 0;
            var held = 0;
            var typed_landed = 0;
            var judged = 0;
            var relanded = 0;
            var hold = 0;
            var hlegs = 0;

            // The episode's instruction, at its first free point.
            action Instruction when (
                pending == 0 && walled == 0 && asked == 0 && typed == 0 && backoff == 0 &&
                landed == 0 && left == 0 && other == 0 && quiet == 0 && held == 0
            ) {
                typed = 1;
                pending = 1;
                relanded = 0;
                hold = 1;
                hlegs = 0;
            }
            // A nudge at every free point after it, with no bound (`Buggy`:
            // a landed task nudged too; its first two counted for the
            // landing that goes quiet after them).
            action Nudge when (
                pending == 0 && walled == 0 && asked == 0 && typed == 1 && backoff == 0 &&
                (landed == 0 || Buggy == 1) && left == 0 && other == 0 && quiet == 0 &&
                held == 0
            ) {
                typed_landed = if landed == 1 { 1 } else { typed_landed };
                nudges = if Buggy == 1 && nudges <= 1 { nudges + 1 } else { nudges };
                pending = 1;
                relanded = 0;
                hold = if landed == 1 { 0 } else if judged == 1 { 2 } else { 1 };
                hlegs = if landed == 1 { 0 } else { hlegs };
            }
            // A question: the land answer — a land act once the instruction
            // was typed.
            action Answer when (
                pending == 0 && walled == 0 && asked == 1 && backoff == 0 && landed == 0 &&
                left == 0 && other == 0 && quiet == 0 && held == 0
            ) {
                asked = 0;
                pending = if typed == 1 { 1 } else { 2 };
                relanded = 0;
                hold = if typed == 0 { 0 } else if judged == 1 { 2 } else { 1 };
                hlegs = if typed == 0 { 0 } else { hlegs };
            }
            // The wall's act, whatever stands: a wall is always acted on.
            action WallAct when (pending == 0 && walled == 1 && landed == 0 && left == 0) {
                walled = 0;
                pending = 3;
            }
            // The decider's wait for the back-off runs out.
            action BackoffDue when (
                pending == 0 && walled == 0 && backoff == 1 && landed == 0 && left == 0 &&
                other == 0
            ) {
                backoff = 0;
            }
            // A land act's yield (or the wall's retry that delivered it):
            // work, a question, a wall — or a claim, landed only where it is
            // a clean defense over a clean repository.
            action YieldWork when (pending > 0) {
                said = 0;
                dirty = 0;
                asked = 0;
                landed = 0;
                told = 0;
                cut = 0;
                short = if pending == 3 { short } else { 0 };
                backoff = if pending == 3 { short } else { 0 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { judged };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    8
                } else {
                    hlegs
                };
            }
            action YieldAsk when (pending > 0) {
                said = 0;
                dirty = 0;
                asked = 1;
                landed = 0;
                told = 0;
                cut = 0;
                short = if pending == 3 { short } else { 1 };
                backoff = if pending == 3 { short } else { 1 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { judged };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    8
                } else {
                    hlegs
                };
            }
            action YieldWall when (pending > 0) {
                walled = 1;
                cut = if pending == 1 { 1 } else if pending == 3 { cut } else { 0 };
                dirty = 0;
                asked = 0;
                landed = 0;
                told = 0;
                short = if pending == 3 { short } else { 0 };
                backoff = if pending == 3 { short } else { 0 };
                pending = 0;
            }
            action YieldClaim when (pending > 0) {
                said = 1;
                dirty = 0;
                asked = 0;
                landed = if Buggy == 1 && (pending == 1 || (pending == 3 && cut == 1)) {
                    1
                } else {
                    0
                };
                told = if Buggy == 1 && (pending == 1 || (pending == 3 && cut == 1)) {
                    1
                } else {
                    0
                };
                cut = 0;
                short = if pending == 3 { short } else { 1 };
                backoff = if pending == 3 { short } else { 1 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { judged };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    8
                } else {
                    hlegs
                };
            }
            action YieldDefense when (pending > 0) {
                said = 2;
                need = 0;
                dirty = 0;
                asked = 0;
                landed = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { 0 };
                told = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { 0 };
                cut = 0;
                short = if pending == 3 { short } else { 1 };
                backoff = if pending == 3 { short } else { 1 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 0 } else { judged };
                relanded = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { relanded };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 || pending == 1 || (pending == 3 && cut == 1) {
                    0
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 || pending == 1 || (pending == 3 && cut == 1) {
                    0
                } else {
                    hlegs
                };
            }
            action YieldDefenseDirty when (pending > 0) {
                said = 2;
                need = 0;
                dirty = 1;
                asked = 0;
                landed = if Buggy == 1 && (pending == 1 || (pending == 3 && cut == 1)) {
                    1
                } else {
                    0
                };
                told = if Buggy == 1 && (pending == 1 || (pending == 3 && cut == 1)) {
                    1
                } else {
                    0
                };
                cut = 0;
                short = if pending == 3 { short } else { 1 };
                backoff = if pending == 3 { short } else { 1 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { judged };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hlegs
                };
            }
            action YieldDefenseNeeds when (pending > 0) {
                said = 2;
                need = 1;
                dirty = 0;
                asked = 0;
                landed = if Buggy == 1 && (pending == 1 || (pending == 3 && cut == 1)) {
                    1
                } else {
                    0
                };
                told = if Buggy == 1 && (pending == 1 || (pending == 3 && cut == 1)) {
                    1
                } else {
                    0
                };
                cut = 0;
                short = if pending == 3 { short } else { 1 };
                backoff = if pending == 3 { short } else { 1 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { judged };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    1
                } else {
                    hlegs
                };
            }
            action YieldOpenDefense when (pending > 0) {
                said = 3;
                need = 0;
                dirty = 0;
                asked = 0;
                landed = 0;
                told = 0;
                cut = 0;
                short = if pending == 3 { short } else { 1 };
                backoff = if pending == 3 { short } else { 1 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { judged };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    4
                } else {
                    hlegs
                };
            }
            // A LIVE READ OF SOMEONE ELSE'S TURN (no act of the policy's
            // awaited), before its end: the face is open again, told at this
            // read (`Buggy`: told nothing).
            action OtherTurnRuns when (pending == 0 && walled == 0 && other == 0) {
                other = 1;
                told = if Buggy == 1 { told } else { 0 };
                judged = 0;
                hold = if typed == 1 && relanded == 0 { 1 } else { 0 };
                hlegs = 0;
            }
            // Someone else's turn of real work ends — on progress, or on a
            // defense, which lands nothing outside a land act's yield: the
            // task is open again, and the person has answered any need.
            action HumanWork when (pending == 0 && walled == 0) {
                said = 0;
                need = 0;
                dirty = 0;
                asked = 0;
                cut = 0;
                short = 0;
                backoff = 0;
                landed = 0;
                left = 0;
                other = 0;
                told = 0;
                judged = 0;
                hold = if typed == 1 && relanded == 0 { 1 } else { 0 };
                hlegs = 0;
            }
            action OtherTurnDefense when (pending == 0 && walled == 0) {
                said = 2;
                need = 0;
                dirty = 0;
                asked = 0;
                cut = 0;
                short = 0;
                backoff = 0;
                landed = 0;
                left = 0;
                other = 0;
                told = 0;
                judged = 0;
                hold = if typed == 1 && relanded == 0 { 1 } else { 0 };
                hlegs = 0;
            }
            // A PERSON'S OWN EXCHANGE: their quick message where the policy
            // carries no task (the worker claimed done, or the task landed),
            // answered — theirs.
            action PersonExchange when (
                pending == 0 && walled == 0 && left == 0 && (said > 0 || landed == 1)
            ) {
                said = 0;
                need = 0;
                dirty = 0;
                asked = 0;
                cut = 0;
                backoff = 0;
                landed = 0;
                left = 1;
                other = 0;
                told = 2;
                judged = 0;
                hold = 0;
                hlegs = 0;
            }
            // Someone else's turn ends on a wall: no claim, no landing; the
            // streak stands.
            action OtherTurnWall when (pending == 0 && walled == 0) {
                said = 0;
                dirty = 0;
                asked = 0;
                walled = 1;
                cut = 0;
                backoff = short;
                landed = 0;
                left = 0;
                other = 0;
                told = 0;
                judged = 0;
                hold = if typed == 1 && relanded == 0 { 1 } else { 0 };
                hlegs = 0;
            }
            // A new loop reads the task record's landing: taken on where the
            // point could be one (`Buggy`: at a wall too).
            action SeedLanded when (
                pending == 0 && (walled == 0 || Buggy == 1) && need == 0 && left == 0 &&
                other == 0
            ) {
                landed = 1;
                said = 2;
                dirty = 0;
                told = 1;
                judged = 0;
                relanded = 1;
                hold = 0;
                hlegs = 0;
            }
            // `Buggy`: the landing spent after two nudges goes quiet.
            action SpentAfterTwo when (
                Buggy == 1 && nudges == 2 && pending == 0 && walled == 0 && landed == 0 &&
                left == 0 && other == 0 && quiet == 0
            ) {
                quiet = 1;
            }
            // `Buggy`: a standing need holds the point for the person.
            action AskHolds when (
                Buggy == 1 && need == 1 && pending == 0 && walled == 0 && landed == 0 &&
                left == 0 && other == 0 && held == 0
            ) {
                held = 1;
            }

            invariant LandNeverGoesQuiet: quiet == 0;
            invariant LandedOnlyByCleanDefense:
                landed == 0 || (said == 2 && need == 0 && dirty == 0);
            invariant ANeedNeverBlocks: held == 0;
            invariant NeverTypeIntoALandedTask: typed_landed == 0;
            invariant AWallIsNeverLeftOnALandedTask: walled == 0 || landed == 0;
            invariant TheHostIsToldTheTasksFace:
                (other == 1 && told == 0) ||
                (other == 0 && left == 1 && told == 2) ||
                (other == 0 && left == 0 && landed == 1 && told == 1) ||
                (other == 0 && left == 0 && landed == 0 && told == 0);
            invariant TheHostIsToldWhatHoldsTheLanding:
                (hold == 0 && hlegs == 0 &&
                    (typed == 0 || relanded == 1 || (other == 0 && (landed == 1 || left == 1)))) ||
                (typed == 1 && relanded == 0 && (other == 1 || (landed == 0 && left == 0)) &&
                    ((hold == 1 && judged == 0 && hlegs == 0) ||
                        (hold == 2 && judged == 1 &&
                            (cut == 1 ||
                                hlegs == (if said <= 1 { 8 } else { 0 }) +
                                    (if said == 3 { 4 } else { 0 }) +
                                    (if said == 2 && need == 1 { 1 } else { 0 }) +
                                    (if dirty == 1 { 2 } else { 0 })))));
        }
    }
}

/// LAND NEVER RESTS SHORT OF ITS LANDING ([`supervisor_land_model`]): `[]<>(a
/// land act's or a wall act's turn runs \/ the task is landed \/ it is a
/// person's own exchange \/ someone else's turn runs)` — on every fair
/// behaviour the landing never sits at a free point with nothing typed for
/// good. Fairness assumed, and nothing more: the loop acts once an act is
/// enabled (`Instruction`, `Nudge`, `Answer`, `WallAct`) and the clock moves
/// the back-off on (`BackoffDue`). The worker is NOT fair: it may claim done
/// for ever, and every claim earns its back-off. Broken alone by
/// `SpentAfterTwo` (the retired two-nudge bound: quiet for good after them)
/// and by `AskHolds` (a need that holds the point for the person, who may
/// never come).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(supervisor_land_model)]
pub fn supervisor_land_liveness() -> Liveness {
    use aterm_spec::derive::{eq, int, or_, var};
    Liveness {
        name: "LandNeverRestsShortOfItsLanding",
        goal: or_(
            or_(
                or_(eq(var("pending"), int(1)), eq(var("pending"), int(2))),
                eq(var("pending"), int(3)),
            ),
            or_(
                or_(eq(var("landed"), int(1)), eq(var("left"), int(1))),
                eq(var("other"), int(1)),
            ),
        ),
        weak: vec!["Instruction", "Nudge", "Answer", "WallAct", "BackoffDue"],
        strong: vec![],
        mutants: vec!["SpentAfterTwo", "AskHolds"],
    }
}

/// The stall's remedy (aterm-agent `supervise/stall.rs`, `Session::stall_remedy`; D4 of
/// `docs/AUDIT-claude-harness-2026-09-23.md`). A worker the server says is frozen is ended
/// for its relaunch, only with nobody there and its bound passed, and once an episode; the
/// loop never kills. `Term` is that one act, `restart pid=<leader> if-stalled quiet=<grace>`:
/// the server stamps the agent before it signals it and ends a survivor itself
/// (`HarnessRestartStamp`); a server that does not know the verb gets the bare
/// `signal term`.
///
/// `stalled` 0 the worker reads its input, 1 frozen (`input=stalled`), 2 a
/// stopped job (`input=stopped`: its `signal cont` is a person's); `aged`
/// the stall has stood `[harness] stall_term_after_s`; `off` that bound is
/// `0`; `hand` a person's hand within `human_grace_s`, a lease or a named
/// turn (`hand=`); `host` the loop's host relaunches an agent the remedy
/// ends (`relaunch`); `termed` this episode's term is sent; `terms` the
/// terms sent this episode; `wrong` a term sent under a hand; `killed` a
/// kill sent. `Term` is the remedy; every other action is the world.
/// `beside` the loop watches beside a PROVEN SELF-CLAIM (the codex-alab
/// contract, §4: the agent supervises itself and the watch-only worker keeps
/// aterm's stall remedy beside it), and `rest` the agent's own session
/// record names its idle point (`status: "idle"`, `draftEmpty: true`;
/// aterm-agent `supervise/beside.rs`). Neither moves `Term`: the stall alone
/// decides, beside a self-claim as for every agent — a TUI that stopped
/// reading its input mid-turn keeps a record that says `busy` for ever, so
/// a remedy that waited on its rest would never come (amended 2026-10-16; `withheld` a remedy so held back).
/// `Buggy = 1`
/// adds four dead actions, each caught alone by an invariant of its own: a
/// kill a minute after the term (`Kill`), a term blind to a person's hand
/// (`TermBlind`), one that forgets it already signalled (`TermAgain`), and
/// one beside a self-claim that waits on its record's rest
/// (`WaitsForTheRest`: the frozen agent never restarted, nobody warned).
/// Tier-1 (aterm-agent `supervise/stall_tests.rs`,
/// `tier1_the_real_remedy_signals_exactly_where_the_model_terms`) drives the
/// real loop over a frozen worker in every reachable configuration and
/// checks it sends `signal term` exactly where `Term` is enabled, once.
/// `Term` is the remedy only under `[harness] stall_term = true`; by default
/// the same gate takes a WARNING instead (the agent left running), which the
/// same Tier-1 checks where `Term` is enabled.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_stall_remedy_model() -> Model {
    aterm_spec::ty_model! {
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
            var beside = 0;
            var rest = 0;
            var withheld = 0;

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
            action BesideFlips when (termed == 0) {
                beside = if beside == 1 { 0 } else { 1 };
            }
            action RestFlips when (termed == 0) {
                rest = if rest == 1 { 0 } else { 1 };
            }
            action Term when (stalled == 1 && aged == 1 && off == 0 && host == 1
                && hand == 0 && termed == 0) {
                terms = terms + 1;
                termed = 1;
            }
            action WaitsForTheRest when (Buggy == 1 && stalled == 1 && aged == 1 && off == 0
                && host == 1 && hand == 0 && termed == 0 && beside == 1 && rest == 0) {
                withheld = 1;
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
            invariant TheStallAloneDecides: withheld == 0;
        }
    }
}

/// A turn end the host let go is decided again (aterm-agent `supervise/run.rs`:
/// `Session::host_held`, `wait_for_next`; D3 of `docs/AUDIT-claude-harness-2026-09-23.md`).
/// The loop decides each point once, as it comes; the session's host (`IdleHost`) may
/// meanwhile own the turn ends (`IdleHost::owns_turn_end`: the live upgrade's wind-down)
/// and take its own step in the point's act's place. Once the host owns nothing, the point
/// left to it is decided again, never waited on with nobody deciding it.
///
/// `point` 0 while the agent works, 1 a turn end the loop decided (act taken or wait
/// running), 2 one left to the host (decided while it owned the turn ends, or passed over
/// for a step that typed and ended nothing: `HostKeeps` still owns, `HostLetsGo` owns
/// nothing), 3 one a host step moved (`HostMoves`: it typed into the agent or ended it; the
/// screen may still show the old point, but its turn is under way). `owns` the host owns
/// the turn ends (`Lapse` ends it); `stranded` the loop waited on a point left to a host
/// that owns nothing; `stale` the loop decided a point a host step had moved. `Buggy = 1`
/// waits on each point whoever owns it and decides again whatever a step left; both are
/// caught. Tier-1: aterm-agent `supervise/run_engine_tests.rs`
/// (`a_turn_end_the_host_let_go_is_decided_again`) checks the real loop continues exactly
/// where `Redecide` is enabled.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_host_turn_end_model() -> Model {
    aterm_spec::ty_model! {
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

/// A person's grace over a READY answer ends in the upgrade's restart, never in the loop's
/// continuation (aterm-agent `harness/upgrade_drive.rs`: `owns_turn_ends`,
/// `attended_owned`, `after_attended`; the window's host `WorkerIdle::at_idle` and its
/// `UpgradeRun` counts; the loop's `decide_turn_end` under `TurnEndReading::upgrading`).
/// While a person's grace holds the restart back (`wait:attended`), the upgrade owns the
/// session's turn ends; otherwise, at the grace's lapse, the loop types `keep going` over
/// the READY before the upgrade looks again, and the agent may never be idle again.
///
/// Before the READY (`point` 0) the host sees the agent's other turn ends: an answer
/// without READY (`HostWaitsBefore`, `wait:awaiting-ready`) or a re-ask a person at the tab
/// holds (`HostHoldsBefore`, `wait:attended`). The READY comes with a turn of the agent's
/// (`ReadyArrives`) and its point stands (`point` 1) until acted on (`point` 2). The host
/// steps first at a point (`HostLooks`, `looked`): a look that finds the person's grace
/// running (`person` 1) waits `attended` and owns the turn ends while its backstop count
/// (`count`, for a stamp that fails closed) is within `Bound`; one that finds it lapsed
/// takes the restart (`HostActs`). The loop continues the agent (`LoopContinues`) only
/// where nobody's grace holds and the host owns nothing. `here` is the attended looks at the
/// READY's point (the spec's measure of the backstop, whatever the host counts); `over`:
/// the loop continued over the READY while that had not run out.
///
/// The host's count is the attended looks in a row at one point: any other wait, and the
/// READY's own turn, start it over. Defects, each caught on its own dial: `Buggy = 1` an
/// attended wait owns nothing; `Every = 1` the backstop reads every wait since the last act
/// (the notice's `awaiting-ready` turn ends among them), so a READY after enough of them
/// owns nothing at its first look; `Stretch = 1` the READY's turn does not start the count
/// over.
///
/// Tier-1: aterm-gui
/// `harness_host::tests::a_person_at_the_tab_leaves_the_point_to_the_upgrade_for_their_grace`
/// drives the real worker (`owns` projected at every look, `Bound` the real backstop's
/// looks), and aterm-agent's `supervise::policy::turn_end` test
/// `a_point_the_upgrade_owns_is_never_continued_at_the_grace_lapse` binds `LoopContinues`'
/// guard to the real policy.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn upgrade_attended_turn_end_model() -> Model {
    aterm_spec::ty_model! {
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

/// The loop's own act, once the agent has taken it, never withholds a break from its host
/// (aterm-agent `supervise/run.rs`: `Session::host_steps_in_background` and the live-turn
/// read in `await_turn_from`; `supervise/policy/turn_end.rs`: `TurnEndState::act_untaken`
/// and `took`). The live upgrade reaches an agent that orchestrates all day only at the
/// breaks of its background work (`IdleHost::at_background`); the loop withholds a break
/// only while an act of its own is untaken. Asking instead whether the act's point was
/// judged withholds every break behind a `keep going` until an idle point an always-busy
/// agent never reaches, since a break reads busy.
///
/// `screen` 0 an idle point, 1 a read of the agent's live turn (spinner, busy footer), 2 a
/// settled break of its background work; `act` 0 none awaited, 1 typed and not taken (no
/// live turn read since: it may sit queued behind work the reader missed), 2 taken, its
/// point not judged yet. `Offer` hands the break to the host, `Withhold` refuses it.
/// `stacked`: a break offered over an untaken act (the guard's purpose: no host line
/// stacked on a continuation the agent has not read); `withheld`: a break refused after the
/// act was taken. Dials, each caught alone (`Buggy = 1`: both): `Judged = 1` the guard asks
/// whether the act's point was judged; `Blind = 1` it asks nothing of the act.
///
/// Tier-1: aterm-agent `supervise/run_engine_tests.rs`
/// (`a_break_after_the_loops_own_taken_act_is_offered_to_the_host`) drives the real loop
/// over scripted screens; it offers the break exactly where `Offer` is enabled.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_break_offer_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorBreakOffer {
            const Buggy = 0;
            const Judged = 0;     // 1: the guard asks for the act's point judged
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

/// The point the upgrade acts at next is owned through its settle (aterm-gui
/// `harness_host.rs`: `UpgradeRun`, `WorkerIdle::at_idle`/`at_background`; aterm-agent
/// `harness/upgrade_drive.rs`: `owns_turn_ends`, `bounded_wait`). The restart after a READY
/// first waits out a short settle (`wait:settling`: Claude's status and the screen quiet
/// `QUIET_S`), and the upgrade owns the session's turn ends through it for
/// `OWNED_SETTLE_LOOKS` looks, so the loop types nothing over the point it is about to act
/// at. That ownership is bounded by the host's count of the settle and drain waits since
/// its last act: an act starts it over, a turn of the agent's never does (a self-waking
/// agent must not renew it). A notice typed at a break is an act too; if it left the count
/// standing, a READY after enough waits would own nothing at its first look.
///
/// `point` 0 before the READY, 1 its point standing, 2 acted on or taken by the loop;
/// `noticed` a notice is out; `waits` the host's count; `looks` its looks at the READY's
/// point; `owns` what its last look left. `SettleBefore` is a counted settle wait before
/// the notice; `NoticeAtIdle`/`NoticeAtBreak` are acts; `OtherWait` (a person at the tab,
/// an answer without READY, a resting round) owns nothing and is no settle or drain.
/// `over`: the loop continued over the READY while its settle still had looks to run.
/// Dials, each caught alone (`Buggy = 1`: both): `Carry = 1` a notice at a break leaves the
/// count standing; `Every = 1` every wait counts.
///
/// Not modelled: a re-ask whose own settle outlived its owned looks at a point the loop then
/// took leaves its settle waits standing, so a READY to the notice before it owns nothing
/// at its first look; the next re-ask, an act, starts the count over.
///
/// Tier-1: aterm-gui `harness_host::tests::a_ready_after_notices_at_breaks_owns_its_settle`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn upgrade_settle_turn_end_model() -> Model {
    aterm_spec::ty_model! {
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

/// A notice its own fence refused owns its point, bounded (aterm-agent
/// `harness/upgrade_drive.rs`: `refused`, `owns_turn_ends`, `after_refused`,
/// `OWNED_REFUSED_LOOKS` and the notice's in-visit tries; aterm-gui `harness_host.rs`:
/// `UpgradeRun`'s count of refused looks and `UpgradeRun::worded`). A notice due at an idle
/// point can be refused by the host's `turn` fence (`if-gen=` saw the screen move:
/// `announce-refused:changed`). Each refused look owns the session's turn ends while the
/// refused looks since the upgrade's last act or the owner's last word are within `Bound`,
/// and is looked at again at the first rung, so the notice is retried at the very point it
/// is due; past the bound the point is the loop's. A turn of the agent's never renews the
/// count; an act (the notice typed) or the owner's word starts it over.
///
/// `point` 1 the idle point standing, 0 the agent's turn; `due` the notice still to be
/// typed; `count` the host's count; `refusals` the refused looks since the last act or word
/// (what the count must be); `owns` what the last look left. `Refused` is a look whose fence
/// refused; `Types` one whose notice went (an act; the notice owns the turn ends);
/// `SettleBefore` one whose notice gate settled (its bound is the settle machine's);
/// `OwnerWord` the owner's word or a newer build. `over`: the loop continued at a point
/// whose refused notice still had owned looks to run.
///
/// Tier-1: aterm-gui's
/// `harness_host::tests::a_refused_notice_owns_its_point_for_its_looks` drives
/// the real worker along the incident's path — refused looks, a turn, refused
/// looks again — and along settles before the refusals and the owner's word
/// after them, beside the model's.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn upgrade_refused_turn_end_model() -> Model {
    aterm_spec::ty_model! {
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

/// The question answer (aterm-agent `supervise/policy/question.rs`, rule
/// `answer-recommended@v1`; keys sent by `supervise/press.rs` `press_question` under
/// `supervise/approval_loop.rs`'s waits): the keys the supervisor sends to answer Claude
/// Code's question dialog (`↑`/`↓` onto the chosen option, Enter on it, never a digit) land
/// only on the dialog state they were decided on, although the PTY carries keys Claude Code
/// has not read yet, a person's as well as the supervisor's own.
///
/// The screen generation fence alone does not prove that: a key written but not yet read
/// changes nothing on the screen. So each key is split into its write (`PersonKey`,
/// `HarnessMove`, `HarnessEnter`, `HarnessType`), appended to the PTY queue `q1`, `q2`
/// (oldest first: 0 none, 1 a person's key, 2 the supervisor's move one row toward the
/// chosen one, `3 + t` its Enter decided on tab `t`, `4 + Tabs + t` its policy text decided
/// on tab `t`), and its consume by Claude Code against the dialog as it is then
/// (`MoveLands`, `MoveShort`, `EnterTaken`, the person's `PersonMovesOff` …
/// `PersonAnswers`, and `KeyRefused`: dropped by the typeahead window, or read after the
/// dialog closed and typed into the composer, where `↑` recalls history). Two guards keep
/// a supervisor key off a state it was not decided on (`Buggy = 1` drops both):
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
///   philosophy review's P3);
///   `supervise`'s one look tries
///   once and hands the box to its manager. `MaxRetry` bounds the checked
///   space, not the loop: every try is guarded alike, so the proof at each
///   bound is the proof of the next try. So its own earlier key is never
///   ahead of its next one either.
///
/// With both, every supervisor key is written on an empty queue, is the next key read, and
/// meets the dialog as the read showed it. That is the whole of the laws: no Enter in the
/// free-text field or on any row but the chosen one (`NoDigitInTheField`, named for the
/// digit the first design sent; `focus` 1 is every other row), none on a later tab
/// (`NeverAnswersANextTabWithAStaleKey`), none on the review's cancel
/// (`NeverCancelsTheReview`: the tool call is rejected and the model told to stop), none
/// after the dialog closed (`NoKeyIntoTheComposer`). With the policy text (`Text = 1`) two
/// more: the text lands only in the empty field it was decided on
/// (`OwnTextOnlyIntoItsEmptyField`), and only once a tab (`TextNeverWrittenTwice`).
///
/// `tab` is the dialog's tab (`0..Tabs-1` a question, `Tabs` the review, `Tabs + 1`
/// closed; with `Review = 0`, a lone single-select question with no Submit tab, the last
/// answer closes it). `focus` 0 is the chosen row (the recommended option; the review's
/// `1. Submit answers`), 1 any other; `row` the focus row's parity, so a move shows on the
/// next read even onto another unchosen row; `typed` a person's text in the free-text row.
/// A question tab may open off the chosen row (`OpensOffChoice`: the vendor focuses option
/// 1, the recommendation may be option 2). A multi-select toggle is a tab of its own: an
/// Enter whose consume moves on to the next choice, focus left where it was. `fresh` is the
/// loop's last read still being the screen (every visible change clears it, a write spends
/// it); `seen_*` is what that read showed. The decision itself is the pure
/// `answer_question` (Tier-1 in `question_tests.rs`): Enter on the chosen row, a move
/// toward it from any other, nothing (escalated) over a person's text or once the dialog
/// closed.
///
/// The person fence (`PersonFence`): the key names the person count of the read it was
/// decided on (`key|send if-human=`), which the server checks under the write's lock, so a
/// person's key written since that read (`touched`) skips the supervisor's. At 0 the laws
/// are refuted. Not modelled: the fence is not atomic with the person's own write, which
/// takes no terminal lock, so a gesture counted after the check and written before the
/// supervisor's write lands first.
///
/// **One ASSUMPTION, not a mechanism** — a constant at 1 here (at 0 the
/// laws are refuted), so the model says exactly what it rests on:
///
/// * `TakenBeforeStill`: a key written before the screen held still for 2 s
///   was read by Claude Code — taken or refused — by then; the retry rests
///   on it. At 0 the retry answers the next tab with a stale key.
///
/// # The policy text (`Text = 1`)
///
/// The question-policy words `long-term` and `short-term`: on a tab that does not fall
/// back, the supervisor moves the focus onto the free-text row (`Type something.`), writes
/// a fixed policy text there and presses Enter, so the worker decides by the policy. That is
/// the one place a supervisor key may land in the field. At `Text = 0` none of this is
/// reachable.
///
/// * Each tab has its own route (`fallback`). A tab the text is estimated not to fit, one
///   with no free-text row, and every tab of a window that cannot fence a typed answer are
///   answered with their recommended option, keyed as on the recommended path. A dialog
///   may mix such tabs with typed ones. `FitLostOnIt`/`FitLostOffIt` turn a tab before or
///   after its first read, while no text is written and no key is unread; never back (the
///   loop latches `fell_back`). A redraw that changes the route under an unread key is
///   outside the model.
/// * The text is one `send`, one queue item (`HarnessType`). Its consume is `TextLands`
///   (read whole), `TextPartly` (its head dropped by the typeahead window) or `KeyRefused`.
/// * No retry, and once per tab: a second text behind an unread first would land in a
///   field no longer empty. `HarnessType` needs `mine` 0 and `wrote` 0 (the loop's memory
///   of its text on the tab, `OwnTextRemembered`; the real `QuestionMemory::typed`), and
///   sets `retried` to `MaxRetry` as the real loop marks a pending text (a record only:
///   every law holds at `retried = 0`).
/// * `own` is the field holding exactly the supervisor's text, typed into that tab's empty
///   focused field. Anything else is `typed` (a person's text, keys added to the
///   supervisor's, a head-dropped suffix, a text landed in a non-empty field) and hands the
///   question over. `exact` is a person's copy of the policy text on a tab the supervisor
///   wrote nothing on: only the loop's memory tells it from its own. On a text tab the
///   chosen row is the free-text row until own text is there; on a multi-select
///   (`Multi = 1`) the typed row then ticks itself and the `Submit`/`Next` button becomes
///   the chosen row, the focus still in the field.
/// * `NoDigitInTheField` here: an Enter on a typed tab is in the field unless the field
///   holds exactly own text, so `HarnessEnter` there needs `seen_own`; on a fallback tab it
///   is any row but the recommended option. `OwnTextOnlyIntoItsEmptyField` (`astray`): the
///   text is read only on its tab, into its focused empty field.
/// * The multi-select Enter hazard (Claude Code 2.1.283): Enter with the focus in a
///   multi-select's field unticks the typed row and the dialog answers "did not answer",
///   so the loop moves `↓` onto the button and presses Enter there. `EnterWhereTyped = 1`
///   is that hazard as a dial: it breaks `NoDigitInTheField` with no person acting.
/// * Geometry is not modelled: the text wraps the field over more rows, a dialog at the
///   pane's foot grows upward, and `↓` on a multi-select reflows the text. The real loop
///   guards each key on the row as drawn on its read, a changed drawing reads as a change,
///   and a dialog cut at its head reads Unknown and is handed over.
///
/// A second assumption, `OwnTextRemembered` (committed 1): the loop remembers the text it
/// typed, per question. At 0 `NoDigitInTheField` is refuted through a person's exact copy
/// alone (a suffix, the text plus more, and a person's own words are told apart by the
/// field's byte-exact read), and so is `TextNeverWrittenTwice`. A lost memory (a restart,
/// a claim handover) is therefore a stall, never an answer.
///
/// Not modelled, owed to the code's tests: the one `send` is one ordered queue item; the
/// text carries no digit or newline; the reader's one-space join of its wrapped rows gives
/// it back exactly; the fit estimate (`text_fits`; every choice of it is covered); a dialog
/// mixing single- and multi-select tabs (`Multi` is one constant, so such a dialog is bound
/// tab form by tab form). Tier-1: the decider (`policy/question_text_tests.rs`,
/// `tier1_text_mode_decider_chooses_only_what_the_model_enables`, with blind,
/// memory-trusting and multi-as-single deciders as negative controls) and the loop
/// (`supervise/run_question_text_tests.rs`,
/// `tier1_the_policy_text_goes_only_where_the_model_allows`, with a memory seeded over a
/// person's copy as its negative control, and
/// `a_persons_key_between_the_read_and_the_send_skips_the_policy_text`).
///
/// # Two instances
///
/// A law no `Buggy = 1` mutant can break at a model's committed constants is a ghost to the
/// non-vacuity sweep, and the two text laws cannot break at `Text = 0`. So this machine is
/// checked as two models, each with only the laws its constants can break: this one, the
/// policy text (`answer-long-term@v1`, `answer-short-term@v1`: `Text = 1`, single-select
/// committed, `Multi` its dial), and [`supervisor_question_answer_model`], the recommended
/// path, cut from it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_question_text_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorQuestionText {
            const Buggy = 0;
            const Tabs = 2;
            const PersonFence = 1;
            const TakenBeforeStill = 1;
            const Review = 1;
            const MaxRetry = 2;
            const Text = 1;
            const Multi = 0;
            const OwnTextRemembered = 1;
            const EnterWhereTyped = 0;
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
            var own = 0;
            var seen_own = 0;
            var astray = 0;
            var fallback = 0;
            var seen_fallback = 0;
            var wrote = 0;
            var twice = 0;
            var exact = 0;
            var seen_exact = 0;

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
            // Their text in the free-text row: off the chosen row when that is an option;
            // with a policy text on a tab not falling back the free-text row is the chosen
            // row, and their text makes the field not own.
            action PersonTypes when (
                q1 == 1 && tab <= Tabs - 1 && (focus == 1 || (Text == 1 && fallback == 0))
            ) {
                typed = 1;
                own = 0;
                exact = 0;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            // Their text is exactly the policy text: only the loop's memory tells it from
            // its own. On a tab the loop wrote its text on (`wrote`) the field holds that
            // text again and is own by every read; elsewhere it is a person's copy
            // (`exact`).
            action PersonTypesPolicyText when (
                Text == 1 && q1 == 1 && tab <= Tabs - 1 && (focus == 1 || fallback == 0)
            ) {
                typed = 1 - wrote;
                own = wrote;
                exact = 1 - wrote;
                focus = if wrote == 1 && Multi == 1 { 1 } else { focus };
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
                own = 0;
                exact = 0;
                fallback = 0;
                wrote = 0;
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
            // Enter answers the focused row: the tab moves on, the review submits (or, on
            // its cancel, rejects the tool call).
            action EnterTaken when (3 <= q1 && q1 <= 3 + Tabs && tab <= Tabs) {
                stale = if q1 == 3 + tab { stale } else { 1 };
                in_field = if tab <= Tabs - 1 && (
                    focus == 1 || (Text == 1 && fallback == 0 && (own == 0 || typed == 1))
                ) { 1 } else { in_field };
                cancelled = if tab == Tabs && focus == 1 { 1 } else { cancelled };
                tab = if tab == Tabs - 1 && Review == 0 { Tabs + 1 } else { tab + 1 };
                focus = 0;
                row = 0;
                typed = 0;
                own = 0;
                exact = 0;
                fallback = 0;
                wrote = 0;
                q1 = q2;
                q2 = 0;
                fresh = 0;
                opened = 1;
            }
            // The policy text read whole. Into its own tab's empty focused field it is
            // `own` (on a multi-select the row ticks itself and the button becomes the
            // chosen row); anywhere else it is astray, and `typed`.
            action TextLands when (4 + Tabs <= q1 && tab <= Tabs) {
                stale = if q1 == 4 + Tabs + tab { stale } else { 1 };
                astray = if q1 == 4 + Tabs + tab && tab <= Tabs - 1 && focus == 0
                    && typed == 0 && own == 0 { astray } else { 1 };
                own = if q1 == 4 + Tabs + tab && tab <= Tabs - 1 && focus == 0
                    && typed == 0 && own == 0 { 1 } else { 0 };
                typed = if q1 == 4 + Tabs + tab && tab <= Tabs - 1 && focus == 0
                    && typed == 0 && own == 0 { typed } else { 1 };
                focus = if q1 == 4 + Tabs + tab && tab <= Tabs - 1 && focus == 0
                    && typed == 0 && own == 0 && Multi == 1 { 1 } else { focus };
                exact = 0;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            // The typeahead window dropped its head: the field holds a suffix of
            // the text, not the supervisor's own.
            action TextPartly when (
                q1 == 4 + Tabs + tab && tab <= Tabs - 1 && focus == 0 &&
                typed == 0 && own == 0
            ) {
                typed = 1;
                own = 0;
                exact = 0;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            // The tab falls back (`Text = 1`): answered with its recommended option, the
            // focus on it or off it. Only while the loop wrote no text on the tab and no
            // key of its is unread; never back (the loop latches it).
            action FitLostOnIt when (
                Text == 1 && fallback == 0 && tab <= Tabs - 1 && wrote == 0 &&
                own == 0 && typed == 0 && q1 == 0 && q2 == 0
            ) {
                fallback = 1;
                focus = 0;
                fresh = 0;
            }
            action FitLostOffIt when (
                Text == 1 && fallback == 0 && tab <= Tabs - 1 && wrote == 0 &&
                own == 0 && typed == 0 && q1 == 0 && q2 == 0
            ) {
                fallback = 1;
                focus = 1;
                fresh = 0;
            }
            // A question tab drawn with its focus off the chosen row (the vendor's option
            // 1, the recommendation option 2), before any read of it.
            action OpensOffChoice when (opened == 1 && tab <= Tabs - 1) {
                focus = 1;
                row = 1 - row;
                fresh = 0;
            }
            // A supervisor key the dialog does not take: dropped in its typeahead window,
            // or, read after the dialog closed, typed into the composer. One action (as
            // `EnterTaken` sets `in_field`): that consume is reachable only past the
            // guards, so as its own action it would be dead in the committed model, which
            // `ty check --strict-vacuity` rejects.
            action KeyRefused when (2 <= q1) {
                composer = if Tabs + 1 <= tab { 1 } else { composer };
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
                seen_own = own;
                seen_exact = exact;
                seen_fallback = fallback;
                seen_quiet = quiet;
                touched = 0;
                mine = if mine == 1 && tab == seen_tab && focus == seen_focus
                    && row == seen_row && typed == seen_typed
                    && own == seen_own { 1 } else { 0 };
                stilled = if mine == 1 && tab == seen_tab && focus == seen_focus
                    && row == seen_row && typed == seen_typed
                    && own == seen_own { stilled } else { 0 };
                retried = if mine == 1 && tab == seen_tab && focus == seen_focus
                    && row == seen_row && typed == seen_typed
                    && own == seen_own { retried } else { 0 };
            }
            // The read showed the box as keyed, and the screen then held
            // still 2 s (`await idle 2000`): by then its key was read.
            action StillScreen when (
                mine == 1 && stilled == 0 && fresh == 1 &&
                (TakenBeforeStill == 0 || (q1 <= 1 && q2 <= 1))
            ) {
                stilled = 1;
            }
            // Enter on the chosen row. With a policy text, on a tab that does not fall
            // back, only once a read showed the field holding own text (on the field, or a
            // multi-select's button after it). Without memory a person's exact copy reads
            // as own.
            action HarnessEnter when (
                fresh == 1 && q2 == 0 && seen_tab <= Tabs &&
                (seen_typed == 0 || (OwnTextRemembered == 0 && seen_exact == 1)) &&
                (seen_focus == 0 || (EnterWhereTyped == 1 && seen_own == 1)) &&
                (Text == 0 || seen_fallback == 1 || seen_tab == Tabs || seen_own == 1 ||
                    (OwnTextRemembered == 0 && seen_exact == 1)) &&
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
                fresh == 1 && q2 == 0 && seen_tab <= Tabs &&
                (seen_typed == 0 || (OwnTextRemembered == 0 && seen_exact == 1)) &&
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
            // The policy text, written into the focused free-text row the read showed
            // empty on a tab that does not fall back: no retry on the box as keyed, and
            // once per tab (`wrote`, the memory `OwnTextRemembered` names).
            action HarnessType when (
                Text == 1 && seen_fallback == 0 && fresh == 1 && q2 == 0 &&
                seen_tab <= Tabs - 1 &&
                seen_focus == 0 && seen_typed == 0 && seen_own == 0 &&
                (Buggy == 1 || (
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0) &&
                    mine == 0 && (OwnTextRemembered == 0 || wrote == 0)
                ))
            ) {
                q1 = if q1 == 0 { 4 + Tabs + seen_tab } else { q1 };
                q2 = if q1 == 0 { 0 } else { 4 + Tabs + seen_tab };
                twice = if wrote == 1 { 1 } else { twice };
                wrote = 1;
                retried = MaxRetry;
                stilled = 0;
                mine = 1;
                fresh = 0;
            }

            invariant NoDigitInTheField: in_field == 0;
            invariant NeverAnswersANextTabWithAStaleKey: stale == 0;
            invariant NeverCancelsTheReview: cancelled == 0;
            invariant NoKeyIntoTheComposer: composer == 0;
            invariant OwnTextOnlyIntoItsEmptyField: astray == 0;
            invariant TextNeverWrittenTwice: twice == 0;
        }
    }
}

/// The laws only a written policy text can break: at `Text = 0` no text is
/// ever written, so none can fail there.
const QUESTION_TEXT_LAWS: [&str; 2] = ["OwnTextOnlyIntoItsEmptyField", "TextNeverWrittenTwice"];

/// The actions only the policy text enables: each guard needs `Text == 1` or a queue code
/// only `HarnessType` writes, so all are disabled at `Text = 0`.
const QUESTION_TEXT_ACTIONS: [&str; 6] = [
    "HarnessType",
    "TextLands",
    "TextPartly",
    "FitLostOnIt",
    "FitLostOffIt",
    "PersonTypesPolicyText",
];

/// THE QUESTION ANSWER'S RECOMMENDED PATH (`answer-recommended@v1`, `Text =
/// 0`, as committed): [`supervisor_question_text_model`]'s machine with the
/// four laws every key of that path keeps — no Enter in the free-text field
/// or off the chosen row, none on a later tab, none on the review's cancel,
/// none into the composer — and only the actions that can fire there. The
/// policy text's two laws and its six actions (`QUESTION_TEXT_LAWS`,
/// `QUESTION_TEXT_ACTIONS`) are the text model's: at `Text = 0` every one of
/// those actions is disabled in every state, so the reachable space is the
/// same state for state, and a law about them is one nothing here can break.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn supervisor_question_answer_model() -> Model {
    let mut m = supervisor_question_text_model();
    m.name = "SupervisorQuestionAnswer";
    for (name, value) in &mut m.consts {
        if *name == "Text" {
            *value = 0;
        }
    }
    m.invariants
        .retain(|law| !QUESTION_TEXT_LAWS.contains(&law.name));
    m.actions
        .retain(|action| !QUESTION_TEXT_ACTIONS.contains(&action.name));
    m
}

/// The decline's keystrokes (aterm-agent `supervise/press.rs`, `Session::press_decline`): a
/// Bash box's `No` amended with a reason the worker acts on: the focus moved off `1. Yes`
/// (`↓`), Tab on the shut `No` (its input opens), the reason typed into the open empty
/// input, Enter on the input showing exactly the reason, each decided on a fresh read.
///
/// Each key is split into its write (`HarnessDown`, `HarnessTab`, `HarnessText`,
/// `HarnessEnter`, a person's `PersonKey`: appended to the PTY queue `q1`, `q2`, oldest
/// first; 1 `↓`, 2 Tab, 3 the reason, 4 Enter, 5 a person's key) and its consume by Claude
/// Code, which meets the box as it is then. A key decided on a stale state does its damage
/// there, unseen by the screen generation fence. Tab toggles the focused option's input
/// (Claude Code 2.1.282), so a second Tab behind an undrawn first shuts it again; and a
/// closed Select reads typed text as keys, so a digit in the reason (`$1` in
/// `rm -rf $S/$1`) chooses an option.
///
/// The guards (and `Buggy = 1` drops both, leaving the generation fence —
/// `fresh`: no visible change since the read — as the port first shipped
/// it):
///
/// With both, every supervisor key meets the box as the read showed it:
/// `NeverChoosesTheAllow`, `NeverAmendsTheAllow`, `NeverTheBareNo` (the shut refusal, or
/// Enter on the empty input, stops the worker for a person), `NeverSubmitsAnotherText`,
/// `NoKeyIntoTheComposer`. A person's own answers (`PersonAnswers`) flag nothing.
///
/// The person fence (`PersonFence`): the keystroke names the person count of the read it
/// was fenced on (`key|send if-human=`), checked under the server's lock, so a person's key
/// written since the read (`touched`) skips the supervisor's. `PersonFence = 0` (a host that
/// sends no count) is refuted by `aterm-spec/tests/derived_supervise.rs`.
///
/// **The person fence** (`PersonFence`, R3b of the question answer): the
/// loop reads the person's stamp on the read the keystroke
/// is fenced on (`seen_quiet`), and the keystroke names that read's person
/// count (`key|send if-human=`), checked under the server's lock with the
/// write — so a person's key written in the round trip since the read
/// (`touched`) skips the supervisor's. At `PersonFence = 0` — a host that
/// sends no count — it is refuted.
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
    aterm_spec::ty_model! {
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
            // Their Backspace: the input empty again.
            action PersonClears when (q1 == 5 && gone == 0 && open == 1 && focus == 1) {
                text = 0;
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
            // Tab toggles the focused option's input: the refusal's, or on `1. Yes` the
            // allow's own amend.
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

/// The network wall (aterm-agent `supervise/policy/turn_end.rs`, `wall_action`'s arms for
/// an API error that never reached the API and a reply the connection cut off): when the
/// supervisor types into such a wall, given only what the session's host measures of the
/// API's reach.
///
/// The decision: an unreachable network is tried on the short ladder while its reach is
/// unmeasured (`Rung1`, then `Rung2` for every later try: the real 1, 2, 5 min, clamped);
/// it is continued at once the first time the host measures the API reachable again, and
/// later Up acts are spaced by the Up acts before them (a lying measure costs the ladder's
/// count, never a burst); while the host measures it down nothing is typed before `Hold`
/// (the real `down_hold`, 15 min) from its appearance, and one try is made then, so a
/// wrong Down strands no one. A reply cut off is continued at once, then on the same
/// ladder, and under a Down takes the hold. A try that led to real work before the wall
/// came back (the real `progress`) ends the episode and its counts start over.
///
/// `net` is the world (the agent's route works); `verdict` the host's measure (0 unknown:
/// no measure, a custom route, stale, a timeout or TLS failure; 1 down; 2 up), which moves
/// in every direction, truthfully or not (`MeasureLies`, `MeasureWrongDown`). `wall`: 0
/// working, 1 the wall shows, 2 an act in flight. `cause`: 0 unreachable, 1 cut off.
/// `waited`: ticks (minutes) since this appearance, saturating at `Hold`. `tries`, `ups`:
/// the episode's acts and its acts on an Up measure (the real `attempts` and `ups`).
/// Ghosts read by no guard: `quick` (acts at the very appearance), `dacts` (acts while
/// measured down), `dticks` (ticks spent at the episode's wall).
///
/// The invariants are about what an episode accumulates, not one decision (a guard-level
/// law would hold by construction): `AMeasuredOutageIsTypedIntoAtMostOnceAHold` (the k-th
/// act into a measured outage comes at least k holds into its episode, however the
/// measure flaps; it rests on the hold counting from each appearance, which `Carry = 1`
/// breaks with `Act`'s guard untouched) and `AtMostTwoActsAtOncePerEpisode` (one for a cut
/// off reply, one for the API measured back). `Buggy = 1` acts at once at every appearance.
/// Tier-1: aterm-agent `tests/supervise_conformance_network_wall.rs` checks the real
/// `decide_turn_end` and `TurnEndState` type exactly where `Act` is enabled, and otherwise
/// wait until exactly the tick it becomes enabled.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_network_wall_model() -> Model {
    aterm_spec::ty_model! {
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

/// The rating card's `0` (aterm-agent `supervise/run.rs`, `Session::survey`, and its
/// busy-read twin in `await_turn_from`): the harness writes a guarded `0` at a Claude Code
/// rating card only where it is taken and lands nowhere else, and removes a `0` the card
/// dropped only when the card can no longer take it. A person typing never receives a `0`
/// meant for a survey, and the harness never deletes its own dismissal.
///
/// `card`: 0 none, 1 open and young (inside the vendor's 600 ms mount settle, which drops a
/// digit into the composer as text), 2 aged. `hand`: a person's keystroke within
/// `human_grace_s`. `draft`: composer text. `boxed`: an approval box. `zero`: 0 none, 1
/// pending (the vendor's 400 ms debounce may still take it, and any composer change first
/// cancels the take), 2 standing (the card dropped it). `takes`: a pending `0` will be
/// taken. `owed`: a `0` of ours may still be in the composer.
///
/// A press needs an aged card, no hand, no draft, no box and no `0` of ours already (`00`
/// abandons the card and stays in the composer). A stray `0` is backspaced only when
/// standing and the composer's whole text with no person at the keyboard; a refused
/// backspace (`halted`, `busy`, `rate`) leaves it owed. `Buggy = 1` presses in any state,
/// backspaces a pending `0`, and forgets a `0` whose backspace was refused, each caught by
/// its own invariant. Tier-1: aterm-agent `run_engine_tests.rs` projects every `key`
/// request of the real `Session` onto this model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_survey_press_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorSurveyPress {
            const Buggy = 0;
            const Tries = 2;
            var card = 0;
            var hand = 0;
            var draft = 0;
            var boxed = 0;
            var zero = 0;
            var takes = 0;
            var owed = 0;
            var tries = 0;
            var young_zero = 0;
            var hand_zero = 0;
            var draft_zero = 0;
            var stacked_zero = 0;
            var pending_backspace = 0;

            action CardOpens when (card == 0) {
                card = 1;
                tries = 0;
            }
            action SettleElapses when (card == 1) {
                card = 2;
            }
            action CardLeaves when (card > 0) {
                card = 0;
                takes = 0;
            }
            action PersonTypes when (hand == 0) {
                hand = 1;
            }
            action GraceEnds when (hand == 1) {
                hand = 0;
            }
            action DraftTyped when (draft == 0 && zero == 0) {
                draft = 1;
            }
            action DraftGone when (draft == 1) {
                draft = 0;
            }
            action BoxUp when (boxed == 0) {
                boxed = 1;
            }
            action BoxGone when (boxed == 1) {
                boxed = 0;
            }
            action Press when (
                card > 0 && tries <= Tries - 1 &&
                ((card == 2 && hand == 0 && draft == 0 && boxed == 0 && zero == 0) || Buggy == 1)
            ) {
                tries = tries + 1;
                young_zero = if card == 1 { 1 } else { young_zero };
                hand_zero = if hand == 1 { 1 } else { hand_zero };
                draft_zero = if draft == 1 || boxed == 1 { 1 } else { draft_zero };
                stacked_zero = if zero > 0 { 1 } else { stacked_zero };
                takes = if card == 2 { 1 } else { 0 };
                zero = 1;
                owed = 1;
            }
            action DebounceRuns when (zero == 1) {
                card = if takes == 1 { 0 } else { card };
                owed = if takes == 1 { 0 } else { owed };
                zero = if takes == 1 { 0 } else { 2 };
                takes = 0;
            }
            action Backspace when (
                draft == 0 && hand == 0 && (zero == 2 || (Buggy == 1 && zero == 1))
            ) {
                pending_backspace = if zero == 1 && takes == 1 { 1 } else { pending_backspace };
                zero = 0;
                takes = 0;
                owed = 0;
            }
            action BackspaceRefused when (
                draft == 0 && hand == 0 && (zero == 2 || (Buggy == 1 && zero == 1))
            ) {
                owed = if Buggy == 1 { 0 } else { 1 };
            }

            invariant NoZeroInsideTheSettle: young_zero == 0;
            invariant NoZeroUnderAPersonsHand: hand_zero == 0;
            invariant NoZeroOverADraftOrABox: draft_zero == 0;
            invariant NoZeroOverAZero: stacked_zero == 0;
            invariant NoBackspaceOfAPendingZero: pending_backspace == 0;
            invariant NoForgottenZero: zero <= 1 || owed == 1;
        }
    }
}

/// The usage wall resumes (aterm-agent `supervise/policy/turn_end.rs`, the wall arm of
/// `decide_turn_end`): a session at a usage wall is continued once its tokens are back,
/// automatically, in bounded time, on a ladder that never gives up.
///
/// `wall`: 0 the worker works, 1 the wall shows, 2 an act in flight, 3 masked (drawn but
/// read as busy behind a background shell), 4 given up, 5 rows out of view (the tab reads
/// busy on its shell), 6 the notice left an idle screen (Esc on the countdown, a local
/// command) and the loop waits on the named reset. `mem`: the loop still remembers the
/// wall it waits on (across a restart, by its open ledger row). `ran`: a person's turn
/// served at a left wall that the loop did not see. `other`: the screen shows another
/// conversation's history, `/resume`d at the left wall (1) or while the wall showed with
/// its countdown armed (2: Claude Code drops the countdown and draws an older limit with
/// the vendor's own `Usage limit reset · continuing automatically`). `bg`: the agent's
/// background work. `clock`: ticks since the wall; tokens are back at `due` (`ResetAt`, or
/// `Skew` later). `view`: 0 the reset placed where the notice says, 1 in the past (a bare
/// clock read by the session-window rule), 2 a day out (a defect). `tries`, `waited`,
/// `blind`: acts at this wall, ticks since the last, acts before the tokens came.
/// `sitting`: a wall found standing with nothing to date its print. `held`: an own act is
/// inside its take window (30 s in the real policy, under a tick). `unread`: the wall's
/// notice left before any read after the act it answered; the cancel under it
/// (`TurnEndState::left_answer`) is the answer all the same. `credits`: usage credits carry
/// the running turn past the limit, served with no wall drawn (`CreditsServes`).
///
/// A wall found sitting is placed by the session-window rule; its one try is the price
/// `NoBlindAct` allows. A wall that answers a try (`Met`) is a watched print, placed where
/// the notice says. Every act waits for the reset as placed and its rung
/// (`max(reset + grace, since + rung)`). A left notice is waited on to the same reset; a
/// person's turn served there (at or before the reset, `LimitLiftedEarly`) ends the wait;
/// another conversation's history, its served turns and its vendor continuation, is
/// nothing of this limit's. No act is decided inside an own act's take window (`held`),
/// and a wall read there is the loop's from its first reading all the same. `Hidden`,
/// `Leaves`, `OwnAct`, `Unread` and `Credits` are dials, committed 1; each at 0 switches
/// its world steps off.
///
/// `Buggy` defects, each caught alone:
/// * `NoBlindAct`: `MetKeepsTheGuess`, `WallMisplaced`, `LeftReadAsTurnEnd`,
///   `OtherLimitsTurnRead`, `OtherLimitsContinuationRead` (another conversation's vendor
///   continuation read as this limit's), `TakeWindowDropsWall` (a wall read in the take
///   window folded in only at a point), `UnreadLeftDropped` (the cancel read as no limit);
/// * `WallRemembered`: `MemoryLost`, `RetryReadAsTaken`, `UnreadAnswerReadAsTaken`;
/// * `NoWaitOverAServedTurn`: `PersonTurnUnseen`;
/// * `NoStuckWall`: `WallMasked`, `GivesUp`;
/// * `NoWallOnCredits`: `CreditsReadAsWall` (a turn credits carry read as a wall);
/// * `NoDayWait` and the liveness: `OlderLeftPlacedTomorrow` (an older cancelled limit's
///   past clock read as tomorrow's).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_usage_wall_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorUsageWall {
            const Buggy = 0;
            const ResetAt = 2;
            const Grace = 1;
            const Rung1 = 1;
            const Rung2 = 2;
            const Skew = 8;
            const Cap = 12;
            const Hidden = 1;
            const Leaves = 1;
            const OwnAct = 1;
            const Unread = 1;
            const Credits = 1;
            var wall = 0;
            var bg = 0;
            var due = 2;
            var clock = 0;
            var view = 0;
            var tries = 0;
            var waited = 0;
            var blind = 0;
            var mem = 1;
            var sitting = 0;
            var ran = 0;
            var other = 0;
            var held = 0;
            var unread = 0;
            var credits = 0;

            // ---- the world ----
            // Background work starts under a turn: never on a screen the
            // wall's notice left idle (`wall == 6`), where none runs.
            action BackgroundStarts when (bg == 0 && wall <= 5) {
                bg = 1;
            }
            action BackgroundEnds when (bg == 1) {
                bg = 0;
            }
            // A wall appears: on time (the account's tokens come at the
            // reset the notice names) or late (they come `Skew` ticks after).
            action WallAppears when (wall == 0 && credits == 0) {
                wall = 1;
                sitting = 0;
                ran = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
            }
            action WallAppearsLate when (wall == 0 && credits == 0) {
                wall = 1;
                sitting = 0;
                ran = 0;
                blind = 0;
                due = ResetAt + Skew;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
            }
            // A wall found standing (the loop starts at it, or a relaunch redraws it
            // without its done row): nothing dates its print, and the session rule reads
            // a bare clock more than five hours ahead as passed.
            action WallFoundSitting when (wall == 0 && held == 0 && credits == 0) {
                wall = 1;
                sitting = 1;
                ran = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 1;
                tries = 0;
                waited = 0;
            }
            action Tick when (1 <= wall && clock <= Cap - 1) {
                clock = clock + 1;
                waited = if waited <= Rung2 - 1 { waited + 1 } else { waited };
                held = 0;
                unread = 0;
            }

            // The wall's rows leave the screen while the agent's shell keeps
            // the tab reading busy (a scroll back, a `/model` under it), and
            // come back (the `Hidden` dial: 0 switches the world off).
            action WallHides when (Hidden == 1 && wall == 1 && bg == 1) {
                wall = 5;
            }
            action WallShows when (wall == 5) {
                wall = 1;
            }
            // The wall's notice leaves an idle screen (`Leaves` dial): Esc on the vendor's
            // countdown (`Automatic continue cancelled`), or a person's `/usage` or
            // `/model` on a tab with no shell.
            action NoticeLeaves when (Leaves == 1 && wall == 1 && bg == 0) {
                wall = 6;
            }
            // A person's turn at the left wall, served — the account has its
            // tokens back — and over before any read saw it busy (its prompt and
            // reply drawn under the notice; the transcript's newest entry the
            // model's): work since the wall, which ends the wait.
            action PersonTurnServed when (wall == 6 && due <= clock && other == 0) {
                wall = 0;
                held = 0;
                unread = 0;
                tries = 0;
                clock = 0;
                waited = 0;
                view = 0;
                sitting = 0;
                blind = 0;
            }
            // The account gets its tokens back before the reset the notice
            // named (a plan changed, usage bought): a turn served now is served
            // before the reset.
            action LimitLiftedEarly when (wall == 6 && clock <= due - 1) {
                due = clock;
            }
            // A person `/resume`s another conversation at the left wall: the
            // screen redraws its history (an older limit's notice, lifted long
            // ago, with turns served under it) and the host hands over its
            // transcript. Nothing of this limit's.
            action ConversationResumed when (Leaves == 1 && wall == 6 && other == 0) {
                other = 1;
            }
            // A person `/resume`s another conversation while the wall shows, the vendor's
            // countdown armed (Claude Code 2.1.300): Claude Code drops the countdown and
            // redraws that conversation's history, an older limit and the vendor's own
            // continuation of it (`Usage limit reset · continuing automatically`), no `❯`
            // row since. The screen reads idle with no wall; this limit stands.
            action ArmedConversationResumed when (
                Leaves == 1 && wall == 1 && bg == 0 && other == 0
            ) {
                wall = 6;
                other = 2;
            }
            // The limit answers the own act at once, and a person's Esc on the
            // vendor's countdown takes its notice off the screen's end before
            // the loop's first read after the act: the screen it reads shows
            // only the vendor's cancel under the notice, no wall.
            action WallAppearsLeft when (
                Unread == 1 && Leaves == 1 && held == 1 && wall == 0 && bg == 0 && credits == 0
            ) {
                wall = 6;
                unread = 1;
                sitting = 0;
                ran = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
            }

            // A person's prompt at a turn's end (no act in flight) answered at once by the
            // limit, and their Esc on the countdown before the next read: only the cancel
            // under the notice is read.
            action PersonWallLeft when (
                Unread == 1 && Leaves == 1 && held == 0 && wall == 0 && bg == 0 && credits == 0
            ) {
                wall = 6;
                unread = 1;
                sitting = 0;
                ran = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
            }
            // Inside the own act's window, before the first read, a person `/resume`s a
            // conversation ending in an older limit cancelled on its countdown: nothing
            // stands, and nothing dates the notice. Read as the act's answer, its bare
            // clock is placed at most a session window out, so a past reset is passed.
            action OlderLeftResumed when (
                Unread == 1 && Leaves == 1 && held == 1 && wall == 0 && bg == 0 && credits == 0
            ) {
                wall = 6;
                unread = 1;
                sitting = 1;
                ran = 0;
                blind = 0;
                due = 0;
                clock = 0;
                view = 1;
                tries = 0;
                waited = 0;
            }

            // Usage credits carry a turn past the limit (the `Credits` dial):
            // it is served at 100% with no wall drawn; once it ends, the next
            // turn's wall is the world's again.
            action CreditsServes when (
                Credits == 1 && wall == 0 && held == 0 && credits == 0 && bg == 0
            ) {
                credits = 1;
            }
            action CreditsTurnEnds when (credits == 1) {
                credits = 0;
            }

            // ---- the supervisor ----
            // The continue policy's own act at a turn's end (the `OwnAct`
            // dial: 0 switches it off): typed, its take window open.
            action OwnActTyped when (OwnAct == 1 && wall == 0 && held == 0 && credits == 0) {
                held = 1;
            }
            // The first act: `Grace` past the named reset (150 s in the real policy); each
            // after a wall that answered again waits a rung, the last forever. Its take
            // window opens with it.
            action Act when (
                held == 0 &&
                (wall == 1 || ((wall == 5 || wall == 6) && mem == 1)) &&
                ((view == 0 && ResetAt + Grace <= clock) || view == 1) && (
                    tries == 0 ||
                    (tries == 1 && Rung1 <= waited) ||
                    (2 <= tries && Rung2 <= waited)
                )
            ) {
                blind = if clock <= ResetAt - 1 { if blind <= 1 { blind + 1 } else { blind } } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                held = OwnAct;
            }
            // What the act led to: the tokens are back (the worker works), or
            // the wall answers again.
            action Taken when (wall == 2 && due <= clock) {
                wall = 0;
                tries = 0;
                clock = 0;
                waited = 0;
                view = 0;
                sitting = 0;
                blind = 0;
                ran = 0;
                held = 0;
            }
            // ... the wall answers again: the next rung counts from this appearance (the
            // real `since`). A watched print, so the reset is placed where the notice says.
            action Met when (wall == 2 && clock <= due - 1) {
                wall = 1;
                waited = 0;
                view = 0;
                held = 0;
            }
            // ... or answers it at once, no turn run (the stub's 429), inside
            // the act's take window: the same print, its point not judged yet.
            action MetInWindow when (wall == 2 && held == 1 && clock <= due - 1) {
                wall = 1;
                waited = 0;
                view = 0;
            }
            // ... or answers it at once and a person's Esc on the new
            // countdown comes before the loop's first read: only the cancel
            // under the answer's notice is ever read.
            action MetLeftUnread when (
                Unread == 1 && Leaves == 1 && wall == 2 && held == 1 && clock <= due - 1 && bg == 0
            ) {
                wall = 6;
                unread = 1;
                waited = 0;
                view = 0;
            }

            // ---- the defects (dead at the committed config) ----
            // The wall drawn behind background work, read as busy: no point. No lift is
            // modelled (a defect action must be caught alone).
            action WallMasked when (Buggy == 1 && wall == 0 && bg == 1 && credits == 0) {
                wall = 3;
                sitting = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
            }
            // The loop forgets the wall it waits on when its rows leave (a
            // restart, an update): nothing remembers it, and no point forms —
            // and a left notice's wait, its ledger row closed at the idle
            // screen, is lost the same way.
            action MemoryLost when (Buggy == 1 && (wall == 5 || wall == 6) && mem == 1) {
                mem = 0;
            }
            // A left notice read as a turn's end: the continue policy types
            // into the standing limit at once, before its reset.
            action LeftReadAsTurnEnd when (Buggy == 1 && wall == 6 && clock <= ResetAt - 1) {
                blind = if blind <= 1 { blind + 1 } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                unread = 0;
            }
            // A person's served turn at the left wall that no read saw busy,
            // read as no turn: the wait stands over it, and the person's
            // continuation is held to the reset (hours; days at a weekly limit).
            action PersonTurnUnseen when (Buggy == 1 && wall == 6 && due <= clock && other == 0 && ran == 0) {
                ran = 1;
            }
            // Another conversation's older limit, and a turn served under it,
            // read as a turn served since this limit: the wait ends while the
            // tokens are not back, and the continue policy types into the
            // standing limit.
            action OtherLimitsTurnRead when (Buggy == 1 && wall == 6 && other == 1 && clock <= due - 1) {
                blind = if blind <= 1 { blind + 1 } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                unread = 0;
            }
            // Another conversation's older limit continued by the vendor,
            // its word read as this limit's continuation (evidence tied to no
            // limit): the wait ends while the tokens are not back, and the
            // continue policy types into the standing limit.
            action OtherLimitsContinuationRead when (
                Buggy == 1 && wall == 6 && other == 2 && clock <= due - 1
            ) {
                blind = if blind <= 1 { blind + 1 } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                unread = 0;
            }
            // A wall read inside the own act's take window folded in only at
            // a point: its notice gone by the window's end, the left limit
            // reads as the act's short turn, and the continue policy types
            // into the standing limit before its reset.
            action TakeWindowDropsWall when (Buggy == 1 && wall == 6 && held == 1 && clock <= ResetAt - 1) {
                blind = if blind <= 1 { blind + 1 } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                held = 0;
                unread = 0;
            }
            // The wall's answer to a retry, read inside the retry's window,
            // not kept as its answer: its notice gone by the window's end,
            // the retry reads as taken and the wall is forgotten (the continue
            // policy types into the standing limit at its back-off).
            action RetryReadAsTaken when (Buggy == 1 && wall == 6 && held == 1 && 1 <= tries && mem == 1) {
                mem = 0;
            }
            // The limit that answered the own act, its notice gone before any
            // read, read as no limit (only the vendor's cancel under it was
            // read): the act's window ends on its short turn, and the
            // continue policy types into the standing limit before its reset.
            action UnreadLeftDropped when (
                Buggy == 1 && wall == 6 && unread == 1 && tries == 0 && clock <= ResetAt - 1
            ) {
                blind = if blind <= 1 { blind + 1 } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                held = 0;
                unread = 0;
            }
            // That older cancelled limit placed on the print of the act it
            // was taken to answer: its past clock read as tomorrow's, no act
            // for a day while the session is free.
            action OlderLeftPlacedTomorrow when (
                Buggy == 1 && Unread == 1 && Leaves == 1 && held == 1 && wall == 0 && bg == 0
                    && credits == 0
            ) {
                wall = 6;
                unread = 1;
                sitting = 1;
                ran = 0;
                blind = 0;
                due = 0;
                clock = 0;
                view = 2;
                tries = 0;
                waited = 0;
            }
            // A retry's answer, its notice gone before any read, not read as
            // its answer: the retry reads as taken and the wall is forgotten.
            action UnreadAnswerReadAsTaken when (
                Buggy == 1 && wall == 6 && unread == 1 && 1 <= tries && mem == 1
            ) {
                mem = 0;
            }
            // The loop gives up after three tries.
            action GivesUp when (Buggy == 1 && wall == 1 && 3 <= tries) {
                wall = 4;
            }
            // The wall answers the act and the loop keeps its guess: the ladder types into
            // the standing wall every rung until the reset.
            action MetKeepsTheGuess when (Buggy == 1 && wall == 2 && clock <= due - 1) {
                wall = 1;
                waited = 0;
            }
            // A bare clock time read by the session window's rule: the reset
            // is placed in the past and the first act goes at once.
            action WallMisplaced when (Buggy == 1 && wall == 0 && credits == 0) {
                wall = 1;
                sitting = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 1;
                tries = 0;
                waited = 0;
            }

            // A turn usage credits serve read as a wall (credits taken for
            // spend): the session held at a limit that did not stop it.
            action CreditsReadAsWall when (Buggy == 1 && credits == 1 && wall == 0) {
                wall = 1;
                sitting = 0;
                ran = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
            }

            // No act before the tokens are back but the one try of a wall
            // found with nothing to date it.
            invariant NoBlindAct: blind <= sitting;
            // Usage credits carrying a turn are no wall.
            invariant NoWallOnCredits: credits == 0 || wall == 0;
            // No wall is masked (`wall == 3`) and the ladder never gives up
            // (`wall == 4`): the defects' marks, each caught alone.
            invariant NoStuckWall: wall <= 2 || 5 <= wall;
            // The loop remembers the wall it waits on.
            invariant WallRemembered: mem == 1;
            // No wait stands over a turn the account served since the wall.
            invariant NoWaitOverAServedTurn: ran == 0;
            // No reset is placed a day out that nothing dated (`view == 2`:
            // the defect's mark).
            invariant NoDayWait: view <= 1;
        }
    }
}

/// The usage wall ends ([`supervisor_usage_wall_model`]): `[]<>(the worker works \/ the
/// tokens are not back yet)`. Fairness assumed: `Tick` (the clock runs while the Mac is
/// awake), `Act`, and `Taken` (the account answers a continuation once it has tokens). The
/// world is not fair: background work may never end and a wall may come late. Broken alone
/// by `WallMasked` (a wall behind background work read as busy), `GivesUp`, `MemoryLost`
/// and `OlderLeftPlacedTomorrow` (an older cancelled limit's past clock placed a day out).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(supervisor_usage_wall_model)]
pub fn supervisor_usage_wall_liveness() -> Liveness {
    use aterm_spec::derive::{eq, int, le, or_, sub, var};
    Liveness {
        name: "TheUsageWallEnds",
        goal: or_(
            eq(var("wall"), int(0)),
            le(var("clock"), sub(var("due"), int(1))),
        ),
        weak: vec!["Tick", "Act", "Taken"],
        strong: vec![],
        mutants: vec![
            "WallMasked",
            "GivesUp",
            "MemoryLost",
            "OlderLeftPlacedTomorrow",
        ],
    }
}

/// THE CREDITS HOLD (aterm-agent `supervise/policy/turn_end.rs`'s
/// `credits_hold`, `TurnEndState::fold_credits`; the owner, 2026-10-19: *"for
/// continuing on credits, that's an optional additional feature that's off by
/// default. use subscriptions."*): usage credits carry a session past its
/// window with no dialog — credits on at the account, the turn that crossed
/// the limit served, Claude Code's `Now using usage credits` drawn — and
/// carry it until the window resets. Under `[harness] usage_credits = false`
/// (the `UsageCredits` dial 0, the default) the supervisor types no act of
/// its own billed to them: it waits for the reset the notice they started on
/// named, and with none named tells its owner once and waits to the hold's
/// bound (`TurnEndTiming::credits_bound`, `Bound` ticks from their start),
/// where one act goes as the probe.
///
/// `point` 1 a turn has ended (0: one runs); `carry` 1 credits carry the
/// session — the world's truth, from the turn that crossed the limit to the
/// window's reset; `notice` 1 the screen says so (the notice can disappear
/// before restoration, or stand at rest past the reset); `known` 1 a notice
/// was read and its episode has not ended on a later no-notice read. No
/// known episode permits an act; a known one with its notice gone still
/// waits to its placed reset or bound. `named` 1 the reset is known — the
/// notice they started on, drawn some ten seconds under the running turn,
/// was read (`CreditsStart`), or not (`CreditsStartUnread`); `clock` the
/// ticks since they started, the window resetting at `ResetAt` — a tick past
/// the vendor's reset by the grace the supervisor keeps, so the reset is the
/// tick its wait ends; `told` the escalations of the episode; `lift` 1 the
/// hold lifts at its end (0: the defects' hold that never does).
/// `AnotherWindow`: the turn served past the reset is on credits still —
/// another window carries them past (the week's under the session's), its
/// reset unknown — told once more where the first was named; with none named
/// it is the same episode to the supervisor until its bound, which starts it
/// over. The window resetting before the bound, the probe's turn may be
/// served on credits where another window binds: `AnotherWindow` after
/// `OwnAct`, the act itself not billed (`carry` 0 when it is typed). A
/// person's turn (`PersonTurn`) is never held.
///
/// `Buggy`: `ActsOnCredits`, the continuation typed while credits carry the
/// session (the policy before the hold), caught by `NoOwnActOnCredits`;
/// `TellsAgain`, the escalation repeated at every point, caught by
/// `ToldOnce`; `StaleNoticeHolds`, the notice at rest past the named reset
/// read as credits still carrying it, the session the subscription serves
/// held, caught by `NoStaleHold` — stated over `OwnAct`'s guard — and by
/// [`supervisor_credits_hold_liveness`]; `UnboundedHold`, the hold with no
/// reset named that never lifts (the policy before its bound, the review of
/// 2026-10-19), caught by `NoEndlessHold`, stated the same way, and by that
/// liveness. The usage wall's `NoWallOnCredits`
/// ([`supervisor_usage_wall_model`]) stands beside it: credits are no wall.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_credits_hold_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorCreditsHold {
            const Buggy = 0;
            const UsageCredits = 0;
            const ResetAt = 2;
            const Bound = 3;
            const Cap = 3;
            var point = 0;
            var carry = 0;
            var notice = 0;
            var known = 0;
            var named = 0;
            var clock = 0;
            var told = 0;
            var billed = 0;
            var lift = 1;

            // ---- the world ----
            // The running turn crosses the window's limit and credits serve
            // it: the notice they started on read, or gone unread.
            action CreditsStart when (point == 0 && carry == 0 && notice == 0 && known == 0) {
                carry = 1;
                notice = 1;
                known = 1;
                named = 1;
                clock = 0;
                told = 0;
                lift = 1;
            }
            action CreditsStartUnread when (point == 0 && carry == 0 && notice == 0 && known == 0) {
                carry = 1;
                notice = 1;
                known = 1;
                named = 0;
                clock = 0;
                told = 0;
                lift = 1;
            }
            // The transient no-reset notice can disappear without showing
            // restoration. A read ends that episode only past its bound.
            action NoticeGone when (notice == 1 && named == 0) {
                notice = 0;
                known = if (named == 1 && ResetAt <= clock)
                    || (named == 0 && Bound <= clock) { 0 } else { known };
            }
            // The same notice can return without restarting its original bound.
            action NoticeReturns when (known == 1 && notice == 0) {
                notice = 1;
            }
            // Time passes while an episode is known, even without its notice;
            // the actual window resets at `ResetAt`.
            action Tick when (known == 1 && clock <= Cap - 1) {
                clock = clock + 1;
                carry = if ResetAt <= clock + 1 { 0 } else { carry };
            }
            // The running turn ends. A subscription answer clears the drawn
            // notice, but does not reveal its unreported reset to the observer.
            action TurnEnds when (point == 0) {
                point = 1;
                notice = if carry == 0 { 0 } else { notice };
                known = if carry == 0
                    && ((named == 1 && ResetAt <= clock)
                        || (named == 0 && Bound <= clock)) { 0 } else { known };
            }
            // The turn served past the reset is on credits still: another
            // window carries them past, its reset unknown. With none named,
            // the supervisor's episode goes on to its bound.
            action AnotherWindow when (point == 0 && notice == 1 && carry == 0) {
                point = 1;
                carry = 1;
                told = if named == 1 { 0 } else { told };
                named = 0;
                clock = if named == 1 || Bound <= clock { 0 } else { clock };
            }
            // A person's own turn: never held.
            action PersonTurn when (point == 1) {
                point = 0;
            }

            // ---- the supervisor ----
            // Its own act at a turn's end: under the key, with no known
            // episode, or at the hold's end — the named reset, else the bound.
            // A missing notice alone does not permit an act.
            action OwnAct when (
                point == 1
                    && (UsageCredits == 1
                        || known == 0
                        || (lift == 1
                            && ((named == 1 && ResetAt <= clock)
                                || (named == 0 && Bound <= clock))))
            ) {
                point = 0;
                billed = if carry == 1 && UsageCredits == 0 { 1 } else { billed };
            }
            // No reset named: the owner told, once an episode, before its
            // bound.
            action Tell when (
                point == 1 && UsageCredits == 0 && known == 1 && named == 0 && told == 0
                    && clock <= Bound - 1
            ) {
                told = 1;
            }

            // ---- the defects ----
            action ActsOnCredits when (Buggy == 1 && point == 1 && carry == 1) {
                point = 0;
                billed = if UsageCredits == 0 { 1 } else { billed };
            }
            action TellsAgain when (
                Buggy == 1 && point == 1 && UsageCredits == 0 && known == 1 && named == 0
                    && told == 1
            ) {
                told = 2;
            }
            action StaleNoticeHolds when (
                Buggy == 1 && point == 1 && UsageCredits == 0 && notice == 1 && carry == 0
                    && named == 1 && lift == 1
            ) {
                lift = 0;
            }
            action UnboundedHold when (
                Buggy == 1 && point == 1 && UsageCredits == 0 && known == 1 && named == 0
                    && lift == 1
            ) {
                lift = 0;
            }

            // Under the key off, no act of the supervisor's own is billed to
            // usage credits.
            invariant NoOwnActOnCredits: UsageCredits == 1 || billed == 0;
            // The owner is told once an episode.
            invariant ToldOnce: told <= 1;
            // A notice at rest past the named reset holds nothing: at such a
            // point, `OwnAct`'s guard holds.
            invariant NoStaleHold: point == 0
                || named == 0
                || clock <= ResetAt - 1
                || UsageCredits == 1
                || known == 0
                || (lift == 1
                    && ((named == 1 && ResetAt <= clock) || (named == 0 && Bound <= clock)));
            // A hold with no reset named lifts at its bound: at a point past
            // it, `OwnAct`'s guard holds.
            invariant NoEndlessHold: point == 0
                || named == 1
                || clock <= Bound - 1
                || UsageCredits == 1
                || known == 0
                || (lift == 1
                    && ((named == 1 && ResetAt <= clock) || (named == 0 && Bound <= clock)));
        }
    }
}

/// THE CREDITS HOLD ENDS ([`supervisor_credits_hold_model`]): `[](point /\ ~carry
/// ~> OwnAct enabled \/ ~known)` as `[]<>`, under the key off — a point the
/// credits no longer carry, the notice still drawn at rest, does not stand
/// held for good: the named reset, else the bound, lifts the hold. Fairness
/// assumed: the clock ticks (`Tick`). Broken alone by `StaleNoticeHolds` (the
/// hold that outlives its named reset) and by `UnboundedHold` (the hold with
/// none named that never lifts, the review of 2026-10-19).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(supervisor_credits_hold_model)]
pub fn supervisor_credits_hold_liveness() -> Liveness {
    use aterm_spec::derive::{and_, eq, int, le, or_, var};
    let own_act = or_(
        eq(var("UsageCredits"), int(1)),
        and_(
            eq(var("lift"), int(1)),
            or_(
                and_(eq(var("named"), int(1)), le(var("ResetAt"), var("clock"))),
                and_(eq(var("named"), int(0)), le(var("Bound"), var("clock"))),
            ),
        ),
    );
    Liveness {
        name: "TheCreditsHoldEnds",
        goal: or_(
            or_(eq(var("point"), int(0)), eq(var("carry"), int(1))),
            or_(eq(var("known"), int(0)), own_act),
        ),
        weak: vec!["Tick"],
        strong: vec![],
        mutants: vec!["StaleNoticeHolds", "UnboundedHold"],
    }
}

/// The model switch (aterm-agent `supervise/policy/turn_end.rs`, the model bucket's arm of
/// `decide_turn_end` and `TurnEndState::restarted`, and the host's restart via
/// `turn_end_loop.rs`'s `restart_agent`): a tab at a model limit moves to the fallback
/// model by a relaunch the host makes when the agent's own jobs allow, and comes back only
/// at a reset the notice named that was still ahead when the switch was made.
///
/// `bucket` the model's limit stands; `named` the notice names a reset (0: an entitlement,
/// `requires usage credits`); `on` the tab is on the fallback; `back` the switch owes a
/// return (`ModelSwitch::back_at`); `jobs` the agent's background work runs (the host
/// refuses the relaunch); `clock` ticks (minutes) since the wall; `probed` the wall was
/// acted on (the continuation at the reset goes first, and a made switch is an act too);
/// `tries` asks the host refused; `flips` relaunches since the worker last did real work;
/// `gaveup`, `held`, `badret` are the defects' marks.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_model_switch_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorModelSwitch {
            const Buggy = 0;
            const ResetAt = 2;
            const Cap = 3;
            const Guess = 1;
            const GiveUpAfter = 2;
            var bucket = 0;
            var named = 0;
            var on = 0;
            var back = 0;
            var jobs = 0;
            var clock = 0;
            var probed = 0;
            var tries = 0;
            var flips = 0;
            var gaveup = 0;
            var held = 0;
            var badret = 0;

            // ---- the world ----
            action JobsStart when (jobs == 0) {
                jobs = 1;
            }
            action JobsEnd when (jobs == 1) {
                jobs = 0;
            }
            action Tick when (bucket == 1 && clock <= Cap - 1) {
                clock = clock + 1;
            }
            // A wall needs a turn that ran: after a relaunch the worker works
            // before it can hit another (the count of relaunches starts over).
            action Works when (bucket == 0 && on == 0 && 1 <= flips) {
                flips = 0;
            }
            action BucketHitsNamed when (bucket == 0 && on == 0 && flips == 0) {
                bucket = 1;
                named = 1;
                back = 0;
                clock = 0;
                probed = 0;
                tries = 0;
                gaveup = 0;
            }
            action BucketHitsUnnamed when (bucket == 0 && on == 0 && flips == 0) {
                bucket = 1;
                named = 0;
                back = 0;
                clock = 0;
                probed = 0;
                tries = 0;
                gaveup = 0;
            }
            // The limit lifts at the reset it named (an entitlement lifts when
            // a person buys it, which the model leaves to the world).
            action BucketLifts when (bucket == 1 && named == 1 && ResetAt <= clock) {
                bucket = 0;
            }

            // ---- the supervisor ----
            // At the reset the continuation goes first: the wall may have
            // lifted, and a switch made at that moment would be for nothing.
            action Continue when (bucket == 1 && on == 0 && named == 1 && ResetAt <= clock && probed == 0) {
                probed = 1;
            }
            // Ask the host for the fallback: made when the agent's jobs are not running,
            // refused (`wait:changed`) while they are, and asked again on the restart
            // back-off, never given up. A switch owes a return only at a named reset still
            // ahead.
            action AskFallback when (
                bucket == 1 && on == 0 && gaveup == 0 &&
                (named == 0 || probed == 1 || clock <= ResetAt - 1)
            ) {
                tries = if tries <= GiveUpAfter { tries + 1 } else { tries };
                on = if jobs == 0 { 1 } else { on };
                probed = if jobs == 0 { 1 } else { probed };
                back = if jobs == 0 { if named == 1 && clock <= ResetAt - 1 { 1 } else { 0 } } else { back };
                flips = if jobs == 0 { if flips <= 3 { flips + 1 } else { flips } } else { flips };
            }
            // The return: only at the reset the switch owes, once it has come,
            // and only when the host can make it. A return the host cannot
            // make now is deferred and holds nothing.
            action Return when (on == 1 && back == 1 && ResetAt <= clock && jobs == 0) {
                on = 0;
                back = 0;
                flips = if flips <= 3 { flips + 1 } else { flips };
            }

            // ---- the defects (dead at the committed config) ----
            // A return with none owed (`Guess` ticks after the switch): back to a wall that
            // stands, and the fallback asked for again.
            action GuessedReturn when (Buggy == 1 && on == 1 && bucket == 1 && back == 0 && Guess <= clock) {
                on = 0;
                flips = if flips <= 3 { flips + 1 } else { flips };
                badret = 1;
            }
            // The asks stop after a few refusals while the wall stands.
            action GivesUp when (Buggy == 1 && on == 0 && bucket == 1 && GiveUpAfter <= tries) {
                gaveup = 1;
            }
            // A deferred return holds the point: nothing else is decided.
            action ReturnBlocks when (Buggy == 1 && on == 1 && back == 1 && ResetAt <= clock && jobs == 1) {
                held = 1;
            }

            invariant NoReturnWithoutAReset: badret == 0;
            invariant NoPingPong: flips <= 3;
            invariant PointsGoOn: held == 0;
            // The asks for the fallback never stop while the wall stands.
            invariant AsksNeverStop: gaveup == 0;
        }
    }
}

/// The fallback is made ([`supervisor_model_switch_model`]): `[]<>(on the fallback \/ the
/// limit lifted \/ the agent's jobs run)`. The jobs are the environment's and not fair: if
/// they run forever the tab stays, asked again at every back-off. Fairness assumed:
/// `AskFallback` and `Continue`. Broken alone by `GivesUp`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(supervisor_model_switch_model)]
pub fn supervisor_model_switch_liveness() -> Liveness {
    use aterm_spec::derive::{eq, int, or_, var};
    Liveness {
        name: "TheFallbackIsMade",
        goal: or_(
            or_(eq(var("on"), int(1)), eq(var("bucket"), int(0))),
            eq(var("jobs"), int(1)),
        ),
        weak: vec!["AskFallback", "Continue"],
        strong: vec![],
        mutants: vec!["GivesUp"],
    }
}

/// The same box is one point (aterm-agent `supervise/run.rs`: a box's review key,
/// `point_key`, and the look that hands a point over). A request standing on the screen is
/// asked of the manager once however the vendor redraws it (re-wrapped, focus moved, its
/// auto-deny countdown ticking, the transcript reflowed above it), and every request the
/// loop looks at is asked, including a second box drawn over the first with no read
/// between (two Edit boxes on one path, different diffs).
///
/// `req` is the request on screen (0 none; 1 and 2 share a head, the same kind and path;
/// 3 is request 2 asked again, which the worker does only after it worked); `seen` how
/// many were drawn; `form` the drawing (a redraw flips it); `key` the loop's review key of
/// what it reads; `handed` the key last handed over; `live` that a read since the last
/// look saw the worker's busy row; `asks` the asks of the request up (counted to 2, one
/// past the bound); `looked` that the loop looked at it. `Look` passes over a point already
/// handed with no live busy read since, and asks anything else.
///
/// `Buggy = 1` turns on three slips, each with its own dial and invariant: `Drawing` keys
/// the drawing (a redraw asks twice, `OneAskPerRequest`); `Coarse` keys the head alone
/// (the second of two Edit boxes on one path is never asked, `EveryLookedRequestIsAsked`);
/// `WritingWorks` takes a read showing only output being written for the worker's turn
/// (the box redrawn through it is asked twice).
///
/// It ASSUMES the one residual no key can see: an identical request comes
/// back only after the worker worked (`live`). Two identical requests queued
/// back to back, with no busy read between them, are one ask. The settle
/// (`box_first_seen`) and the wait's read under a file box's first row are
/// not actions here; the walk below binds the second. Tier-1 (aterm-agent
/// `run_box_key_tests.rs`) drives the real `Session` over the scripted
/// server and walks each read onto these actions: every invariant at every
/// state, the real review key's equalities against `key == handed` at every
/// look, the real file-request comparison against the request at every read
/// the wait makes under a box's row, and each real ask (its badge) raised
/// right after the read the model asks at — with the key before FIX 3 replayed through
/// the same walk as the caught negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_box_ask_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorBoxAsk {
            const Buggy = 0;
            const Drawing = 0;
            const Coarse = 0;
            const WritingWorks = 0;
            var req = 0;
            var seen = 0;
            var form = 0;
            var key = 0;
            var handed = 0;
            var live = 0;
            var asks = 0;
            var looked = 0;

            // The next request drawn, over the last with no read between or
            // after it left; the same request again (3) only after work.
            action NextBox when (seen <= 2 && (seen <= 1 || live == 1)) {
                seen = seen + 1;
                req = seen + 1;
                form = 0;
                asks = 0;
                looked = 0;
                key = if Coarse == 1 || Buggy == 1 {
                    1
                } else {
                    if seen == 2 { 2 } else { seen + 1 }
                };
            }
            // The vendor redraws the request: re-wrapped, focus moved, countdown ticked,
            // the transcript reflowed above it.
            action Redraw when (req > 0) {
                form = 1 - form;
                key = if Drawing == 1 || Buggy == 1 {
                    if form == 1 { key - 10 } else { key + 10 }
                } else {
                    key
                };
            }
            // A read of output being written, the box off the screen a
            // moment (no composer, no busy row, never still).
            action Writing when (req > 0) {
                live = if WritingWorks == 1 || Buggy == 1 { 1 } else { live };
            }
            // A person (or the manager) answers the request: it leaves.
            action Answer when (req > 0) {
                req = 0;
            }
            // The worker works: its busy row read.
            action Work when (req == 0) {
                live = 1;
            }
            // The loop's look at the request up.
            action Look when (req > 0) {
                asks = if handed == key && live == 0 {
                    asks
                } else {
                    if asks <= 1 { asks + 1 } else { asks }
                };
                handed = key;
                live = 0;
                looked = 1;
            }

            invariant OneAskPerRequest: asks <= 1;
            invariant EveryLookedRequestIsAsked: req == 0 || looked == 0 || asks > 0;
        }
    }
}
