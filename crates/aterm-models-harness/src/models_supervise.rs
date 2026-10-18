// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's own bounded machines (aterm-agent `supervise`, lane B2):
//! the session claim it acts under, the focus-then-Enter choice it makes on
//! an unnumbered dialog, the turn-end policy and its done check, the keys it
//! sends to answer Claude Code's question dialog, the keystrokes of a
//! decline, when it types into an API error the network caused, the
//! stall's remedy, and the one ask a box gets however it is redrawn.

use aterm_spec::derive::{Liveness, Model};

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
/// renewal and without its fence — the stale-view press — and is caught: a
/// lapse, another's claim, then a press under it. Tier-1 (aterm-agent
/// `run_engine_tests.rs`) drives the real `Session` over the scripted server
/// (an `External` loop) and projects its claim state onto these variables.
/// A host with no claim to take (`CLAIM unavailable`: a build before lane C)
/// is outside this model: the loop acts there as it did before the claim
/// existed.
///
/// THE PROVEN SELF-CLAIM (the codex-alab native harness contract, §2): the
/// server lets a proven self-claim REPLACE the window host's held claim
/// (`ProvenReplaces`, `held` 1 → 2) while the loop still reads it as its own
/// (`view` 1) — no renewal precedes that. Only the HOSTED loop's claim is so
/// replaced, and it FENCES every write with its own holder
/// (`if-supervisor=<holder>`, `fenced` 1): the server admits a fenced write
/// only while that holder holds the claim, counted from the fence's read to
/// the write's end, and a replacement is answered only once those writes
/// landed (aterm-gui `session_timeline::FencedWrite`), so a fenced press
/// under another's claim lands nothing. An external loop (`External`) does
/// not fence and is never replaced so; it is held to the renewal.
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
/// `decide_turn_end`), FULLY AUTOMATIC: what the
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
/// (typing it is a person's keystroke: its grace runs). `Buggy = 1` ignores
/// the box, the person, the draft and the back-off, escalates "worker
/// reports done" after two short turns, keeps awaiting an unseen point past
/// its deadline (a silent latch), reads the harness's own turn as a task (D1:
/// `keep going` and `answer_text` into a session nobody had asked anything),
/// as a short turn of the worker's (N1), and its answer of real work as no
/// end of the streak — each caught. Tier-1
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
///
/// THE TASK'S FACE (the Autopilot design's §9.4.2), as the loop tells its
/// host (`IdleHost::task_face`): `fin` 1 once the continuation's reply ended
/// on the worker's own token (`WorkedFinished`: the task is DONE, nothing is
/// typed until someone else's turn, and the done report is a short turn of
/// the streak's); `other` 1 once a LIVE READ of someone else's turn came
/// before its end (`OtherTurnRuns`, no act of the policy's awaited — a turn
/// runs, so nothing is typed, and its end is `HumanWork`); `told` the face
/// the host was last told — 0 open, 1 done — which must be the face of the
/// state at every step (`TheHostIsToldTheTasksFace`): open while someone
/// else's turn runs, else done where `fin` says so. A point tells the face it
/// moves to, and so does that live read: told only at points, `✓ done` stood
/// through a person's new task for as long as it ran. `Buggy = 1` tells
/// nothing at the live read — caught. Tier-1 projects the real
/// `TaskFace::of`, `TurnEndState::other_turn` and the face the real teller
/// (`TurnEndState::face_moved`) told onto these, and three tellers that miss
/// it each fail the walk: one told on the journal's `DONE`/`LEFT` notes
/// alone, one told at points alone, and one that says landed for any done
/// task.
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
            var fin = 0;
            var other = 0;
            var told = 0;

            action BoxAppears when (box_up == 0 && pending == 0 && other == 0) {
                box_up = 1;
            }
            action BoxLeaves when (box_up == 1) {
                box_up = 0;
            }
            // (Each clock runs alone — a wall's wait, a back-off, the cap's
            // window — so the real side can move its clock by exactly the
            // one that runs out.)
            action WallAppears when (
                wall == 0 && box_up == 0 && pending == 0 && backoff == 0 && other == 0
            ) {
                wall = 1;
                fin = 0;
                told = 0;
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
            // answered).
            action HumanWork when (pending == 0 && box_up == 0 && wall == 0) {
                short = 0;
                backoff = 0;
                earned = 0;
                long = 1;
                draft = 0;
                asked = 1;
                tasked = 1;
                fin = 0;
                other = 0;
                told = 0;
            }
            // A LIVE READ OF SOMEONE ELSE'S TURN (a person's, an
            // orchestrator's: no act of the policy's awaited), before its
            // end: the task's face is open again, told at this read — not at
            // the turn's end, which may be hours on.
            action OtherTurnRuns when (
                pending == 0 && box_up == 0 && wall == 0 && draft == 0 && other == 0
            ) {
                other = 1;
                told = if Buggy == 1 { told } else { 0 };
            }
            // The harness's own turn — its notice or its carry-on, the
            // upgrade's two — answered short: NO task, and no short turn of
            // the worker's: the streak and its back-off stand.
            action HarnessTurn when (
                harness <= 1 && pending == 0 && box_up == 0 && wall == 0 && draft == 0 &&
                other == 0
            ) {
                harness = harness + 1;
                short = if Buggy == 1 && short <= Streak - 1 { short + 1 } else { short };
                backoff = if Buggy == 1 { 1 } else { backoff };
                tasked = if Buggy == 1 { 1 } else { tasked };
            }
            // …answered with REAL WORK (a carry-on's answer is the worker's
            // own work, resumed): still no task, and the streak ends.
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
                fin = 0;
                told = 0;
            }
            action Continue when (
                pending == 0 && wall == 0 && fin == 0 && other == 0 &&
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
            }
            action WorkedLong when (pending == 1) {
                pending = 0;
                short = 0;
                backoff = 0;
                earned = 0;
                long = 1;
            }
            // The continuation's reply ends on the worker's own token
            // (`ATERM-TASK-DONE`): THE TASK IS DONE — nothing more is typed
            // until someone else's turn — and the host is told so at this
            // point. A done report is a short turn of the streak's.
            action WorkedFinished when (pending == 1) {
                pending = 0;
                short = if short <= Streak - 1 { short + 1 } else { short };
                backoff = 1;
                earned = 1;
                long = 0;
                fin = 1;
                told = 1;
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
                pending == 0 && wall == 2 && other == 0 && (tasked == 1 || Buggy == 1) &&
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
                fin == 0 && other == 0 &&
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
                pending == 0 && box_up == 0 && person == 0 && tasked == 1 && fin == 0 &&
                other == 0 && (
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
            invariant TheHostIsToldTheTasksFace:
                (other == 1 && told == 0) || (other == 0 && told == fin);
        }
    }
}

/// THE DONE CHECK (aterm-agent `supervise/policy/turn_end.rs`). A worker whose last
/// words say it is DONE is not continued: at the point past its back-off it
/// is typed the check — *"do every open item you can still do …; end your
/// reply with ATERM-TASK-DONE … only when what is left needs a person or
/// cannot be done on this host"* — and when that check's own yield says done
/// again (the token on a line of its own anywhere in it, or the worker's words
/// for done; a report only that nothing of its own runs or is queued, or only
/// the check's words echoed, is work going on there, not done) THE TASK IS
/// DONE: nothing more is typed into the session until someone else's turn (a
/// person's, an orchestrator's) opens it again. Without it a finished task is
/// continued on the back-off until the model invents work.
///
/// THE WORKER'S OWN FINISHED (DONE-1, 2026-10-16): a report whose last line is
/// `ATERM-TASK-DONE` ends the task at once, with no check — unless its own
/// words name open work without a blocker (`Still open:`, `Left:`, `not run`):
/// then it is a first done report owing THE CHALLENGE, typed once a job, and
/// a finished or done yield to it ends the task whatever it still lists. The
/// job's memory of the challenge is forgotten when the task ends, so the next
/// job is challenged afresh.
///
/// `done` is the done reports in a row at the points the policy saw: 0 none,
/// 1 the point is one (the check, or the challenge, is owed), 2 the task is
/// done; `pending` 0 at a point, 1 a continuation's turn running, 2 the
/// check's, 3 a wall's act's (the 529's retry, the limit's resume,
/// `/compact`), 4 the challenge's; `walled` the point shows a wall — its
/// ladder acts there, never the continuation, the check or the challenge;
/// `open` the point is a FINISHED naming open work (the challenge is owed);
/// `challenged` the challenge was typed this job. `typed_done` something was
/// typed into a done task, `unchecked` a done report was answered with a
/// plain continuation, `cut_short` the task ended on a done report the check
/// never asked about, `open_end` the task ended on an open FINISHED no
/// challenge asked about, `twice` a second challenge typed in one job.
/// `Buggy = 1` continues a done report like any point and reads it as the end
/// the moment it repeats, and leaves a done task closed when someone else's
/// turn ends on a wall (a person's turn that hit a 529 after the task was done
/// is stranded, no retry) — and adds two dead actions: the harness before
/// DONE-1, which ended the task on an open FINISHED at once
/// (`FinishOpenAtOnce`), and a challenge that forgets it was asked, typing
/// another at the open FINISHED that answers it (`ChallengeAgain`) —
/// caught by each invariant on a path of its own. Tier-1 (aterm-agent
/// `tests/supervise_conformance_done_check.rs`) drives the real
/// `decide_turn_end` and `TurnEndState` along every reachable state and checks
/// it types the continuation, the check, the challenge, the wall's act, or
/// nothing exactly where the model does.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_done_check_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorDoneCheck {
            const Buggy = 0;
            var done = 0;
            var pending = 0;
            var typed_done = 0;
            var unchecked = 0;
            var cut_short = 0;
            var walled = 0;
            var open = 0;
            var challenged = 0;
            var open_end = 0;
            var twice = 0;

            // Someone else's turn, answered: the task is theirs again, and
            // a done report in it is a first one — and one that ended on a
            // wall is no done report at all.
            action OtherTurnDone when (pending == 0) {
                done = 1;
                walled = 0;
                open = 0;
            }
            action OtherTurnWork when (pending == 0) {
                done = 0;
                walled = 0;
                open = 0;
            }
            action OtherTurnWall when (pending == 0) {
                done = if Buggy == 1 { done } else { 0 };
                walled = 1;
                open = 0;
            }
            // Someone else's turn ending on the worker's FINISHED: a clean
            // report ends the task; one naming open work owes the challenge,
            // unless this job already had it. Either way the job's memory of
            // the challenge goes: the task ended, or it was never asked.
            action OtherTurnFinished when (pending == 0) {
                done = 2;
                walled = 0;
                open = 0;
                challenged = 0;
            }
            action OtherTurnOpenFinished when (pending == 0) {
                done = if challenged == 1 { 2 } else { 1 };
                open = if challenged == 1 { 0 } else { 1 };
                walled = 0;
                challenged = 0;
            }
            // The policy's act at a free point: the wall's act at a wall,
            // the continuation where no done was reported, the check where
            // one was, the challenge at an open FINISHED, nothing on a done
            // task.
            action WallAct when (pending == 0 && walled == 1 && done <= 1) {
                walled = 0;
                pending = 3;
            }
            action Continue when (pending == 0 && walled == 0 && (done == 0 || Buggy == 1)) {
                typed_done = if done == 2 { 1 } else { typed_done };
                unchecked = if done == 1 { 1 } else { unchecked };
                pending = 1;
            }
            action Check when (pending == 0 && walled == 0 && done == 1 && open == 0) {
                pending = 2;
            }
            action Challenge when (pending == 0 && walled == 0 && open == 1) {
                twice = if challenged == 1 { 1 } else { twice };
                challenged = 1;
                open = 0;
                pending = 4;
            }
            action YieldWork when (pending > 0) {
                pending = 0;
                done = 0;
            }
            action YieldDone when (pending > 0) {
                cut_short = if pending == 1 && done == 1 { 1 } else { cut_short };
                done = if pending == 2 || pending == 4 || (Buggy == 1 && done == 1) { 2 } else { 1 };
                challenged = if pending == 2 || pending == 4 || (Buggy == 1 && done == 1) {
                    0
                } else {
                    challenged
                };
                pending = 0;
            }
            // A yield ending on the worker's FINISHED: as someone else's turn
            // ending so, the challenge's own yield ending the task.
            action YieldFinished when (pending > 0) {
                pending = 0;
                done = 2;
                challenged = 0;
            }
            action YieldOpenFinished when (pending > 0) {
                done = if challenged == 1 { 2 } else { 1 };
                open = if challenged == 1 { 0 } else { 1 };
                challenged = 0;
                pending = 0;
            }
            // A yield that ended on a wall: its done reports stand.
            action YieldWall when (pending > 0) {
                pending = 0;
                walled = 1;
            }
            // `Buggy`: the harness before DONE-1, which ended the task on an
            // open FINISHED at once.
            action FinishOpenAtOnce when (Buggy == 1 && pending == 0 && open == 1) {
                open_end = 1;
                done = 2;
                open = 0;
                challenged = 0;
            }
            // `Buggy`: a challenge that forgets it was asked — the open
            // FINISHED answering it is challenged again.
            action ChallengeAgain when (Buggy == 1 && pending == 4) {
                twice = 1;
                pending = 4;
            }

            invariant NeverTypeIntoADoneTask: typed_done == 0;
            invariant ADoneReportIsChecked: unchecked == 0;
            invariant OnlyTheCheckEndsTheTask: cut_short == 0;
            invariant AWallIsNeverLeftOnADoneTask: walled == 0 || done <= 1;
            invariant AnOpenFinishIsChallenged: open_end == 0;
            invariant OneChallengeAJob: twice == 0;
        }
    }
}

/// THE STALL'S REMEDY (aterm-agent `supervise/stall.rs`,
/// `Session::stall_remedy`; D4 of the live E2E,
/// `docs/AUDIT-claude-harness-2026-09-23.md`). A worker the
/// server says is frozen is ended for its relaunch, only with nobody there
/// and its bound passed — and ONCE an episode, never a kill of the loop's.
/// Since the explicit restart (design of 2026-10-08) `Term` is that one act,
/// `restart pid=<leader> if-stalled quiet=<grace>`: the server stamps the
/// agent before it signals it and ends a survivor itself (its contract,
/// `HarnessRestartStamp`), so the loop never sends `signal kill`; an older
/// server that does not know the verb gets the bare `signal term`.
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
/// a remedy that waited on its rest would never come (amended 2026-10-16,
/// the review's finding 2; `withheld` a remedy so held back). `Buggy = 1`
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

/// A TURN END THE HOST LET GO IS DECIDED AGAIN (aterm-agent
/// `supervise/run.rs`, `Session::host_held` and `wait_for_next`; D3 of the
/// live E2E, `docs/AUDIT-claude-harness-2026-09-23.md`). The
/// loop decides each point once, as it comes;
/// the session's host (the window's worker, `IdleHost`) may own the
/// session's turn ends meanwhile (`IdleHost::owns_turn_end`: the live
/// upgrade's wind-down) and may take its own step at the point, in the
/// point's act's place. Once the host owns nothing any more, the point it was
/// left to is DECIDED AGAIN — never waited on with nobody deciding it.
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
/// decided a point a host step had moved (e.g. the point after a typed
/// carry-on, decided again while the agent began its answer). `Redecide` is
/// the loop's decision again; `LoopWaits`
/// its wait for the screen to move. `Buggy = 1` is the loop that decides
/// each point once and waits on it whoever owns it, and that decides again
/// whatever a step left behind — both caught. Tier-1 (aterm-agent
/// `supervise/run_engine_tests.rs`, `a_turn_end_the_host_let_go_is_decided_again`)
/// drives the real loop along the model's paths and checks it continues
/// exactly where `Redecide` is enabled.
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

/// A PERSON'S GRACE OVER A READY ANSWER ENDS IN THE UPGRADE'S RESTART, NEVER IN
/// THE LOOP'S CONTINUATION (aterm-agent `harness/upgrade_drive.rs`,
/// `owns_turn_ends`, `attended_owned` and `after_attended`; the window's host
/// `WorkerIdle::at_idle` and its `UpgradeRun` counts; the loop's
/// `decide_turn_end` under `TurnEndReading::upgrading`). The incident: the agent
/// answered READY 38 s after the owner typed "ok", so the upgrade's restart waited
/// on the person (`held-back:attended`, then `wait:attended`) — owning none of the
/// session's turn ends, and looking again on its growing ladder. The loop held off
/// for the same person's grace, and at its lapse nobody owned the point but the
/// loop: it typed `keep going` over the READY, 28 s before the upgrade's next look,
/// and the upgrade then waited 5 h 50 min for an idle point of an agent that was
/// never idle again.
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
/// on its own dial: `Buggy = 1`, an attended wait owns nothing; `Every = 1`, the
/// backstop reads every wait in a row since the last act — the notice's
/// `awaiting-ready` turn ends among them — so a READY after enough of them owns
/// nothing at its first look; `Stretch = 1`, a count the READY's turn does not start over (a
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
/// was withheld until an idle point that an always-busy agent never reaches,
/// so the upgrade got no look for hours, every clock of it frozen.
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
/// `Judged = 1` a guard that asks whether the act's point was judged;
/// `Blind = 1` a guard that asks nothing of the act. `Buggy = 1` both.
///
/// Tier-1: aterm-agent's `supervise/run_engine_tests.rs`
/// (`a_break_after_the_loops_own_taken_act_is_offered_to_the_host`) drives the
/// real loop over scripted screens along the model's paths — the loop's `keep
/// going`, a live turn or none, a settled break — and the real loop offers the
/// break exactly where `Offer` is enabled.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_break_offer_model() -> Model {
    aterm_spec::ty_model! {
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
/// turn of the agent's never does (a self-waking agent must not renew it).
/// The incident: six looks at a person at the tab, then three notices typed
/// at BREAKS that never went through the count, then the READY — whose first
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
/// `Carry = 1` a notice typed at a break leaves the count standing;
/// `Every = 1` every wait counts (the notice's `awaiting-ready` answers and a
/// person's looks spend the settle's looks before the READY comes).
/// `Buggy = 1` both.
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

/// A NOTICE ITS OWN FENCE REFUSED OWNS ITS POINT, BOUNDED (aterm-agent
/// `harness/upgrade_drive.rs`, `refused`, `owns_turn_ends`, `after_refused`,
/// `OWNED_REFUSED_LOOKS` and the notice's in-visit tries; aterm-gui
/// `harness_host.rs`, `UpgradeRun`'s own count of refused looks and
/// `UpgradeRun::worded`). The incident: after the owner's `--now`, at the one
/// idle point the agent offered the notice was due — its gate said go — but
/// the host's `turn` refused it three times (`if-gen=` saw the screen move:
/// `announce-refused:changed`), each a wait that owned nothing on a climbing
/// ladder; then the loop typed `keep going` into the point, and the agent was
/// never idle again. Each refused look owns the session's turn ends while the refused
/// looks since the upgrade's last act or the owner's last word are within
/// `Bound`, and is looked at again at the first rung, so the notice is tried
/// again at the very point it is due; past the bound the point is the loop's.
/// A turn of the agent's never renews the count: an act — the
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
/// count (a round whose gate had settled `Bound` looks owns nothing at its
/// first refusal); `Stale = 1` the owner's word leaves the count standing (an
/// `Upgrade now` pressed after ninety minutes of refusals owns nothing).
/// THE BOUND IS A PATH, NOT AN INVARIANT: `Renew = 1` (a turn of the agent's
/// starts the count over, so every point is owned anew)
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

/// THE QUESTION ANSWER (aterm-agent `supervise/policy/question.rs`, the
/// rule `answer-recommended@v1`; its keys sent by `supervise/press.rs`'s
/// `press_question` under `supervise/approval_loop.rs`'s waits): the keys
/// the supervisor sends to answer Claude Code's question dialog — `↑` or
/// `↓` to move the focus onto the option it chose, Enter on it, never a digit
/// (the critique's R4) — land only on the dialog state they
/// were decided on, although the PTY carries keys Claude Code has not read
/// yet, a person's as well as the supervisor's own (R3c).
///
/// **The screen generation fence alone does not prove that** (R3): it
/// proves the SCREEN did not change between the read a key was decided on
/// and its write, and a key written to the PTY that Claude Code has not
/// read yet changes nothing on the screen. So every key is split here into
/// its WRITE (`PersonKey`, `HarnessMove`, `HarnessEnter`, `HarnessType`:
/// appended to the PTY's queue `q1`, `q2`, oldest first — 0 none, 1 a
/// person's key, 2 the supervisor's `↑` or `↓` (the model abstracts the
/// direction: a move is one row toward the chosen one), `3 + t` its Enter
/// decided on tab `t`, `4 + Tabs + t` its policy text decided on tab `t`,
/// THE POLICY TEXT below) and its CONSUME
/// by Claude Code (the head's effect on the dialog as it is THEN: `MoveLands`,
/// `MoveShort`, `EnterTaken`, the person's `PersonMovesOff` … `PersonAnswers`,
/// `KeyRefused` for a key the dialog does not take: dropped by its
/// typeahead window, O4, or — read after the dialog closed — typed into the
/// composer, where `↑` recalls history). What keeps a
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
///   philosophy review's P3);
///   `supervise`'s one look tries
///   once and hands the box to its manager. `MaxRetry` bounds the checked
///   space, not the loop: every try is guarded alike, so the proof at each
///   bound is the proof of the next try. So its own earlier key is never
///   ahead of its next one either.
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
/// dialog closed (`NoKeyIntoTheComposer`: `↑` recalls history into it). With
/// the policy text (`Text = 1`) a fifth and a sixth: the text lands only in
/// the empty field it was decided on (`OwnTextOnlyIntoItsEmptyField`), and
/// only once on a tab (`TextNeverWrittenTwice`).
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
/// **The person fence** (`PersonFence`, R3b): the loop
/// reads the person's stamp on the read the key is decided on
/// (`seen_quiet`), and the key names that read's person count (`key|send
/// if-human=`), which the server checks under the same lock as the write —
/// so a person's key written in the round trip since the read (`touched`:
/// `PersonKey` sets it, a `Read` clears it) skips the supervisor's. At
/// `PersonFence = 0` — a host that sends no count — the invariants are
/// refuted.
/// Out of the model: the fence is not atomic with the person's own write,
/// which takes no terminal lock — a gesture counted after the server's
/// check and written before the supervisor's write, inside that one hold,
/// lands first.
///
/// **One ASSUMPTION, not a mechanism** — a constant at 1 here (at 0 the
/// laws are refuted), so the model says exactly what it rests on:
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
///
/// **THE POLICY TEXT** (`Text = 1`; the question-policy words `long-term`
/// and `short-term`). On a tab that does not fall back (below) the
/// supervisor chooses no option: it moves the focus onto the question's
/// free-text row (`Type something.`), writes a fixed policy text there and
/// presses Enter on it, so the worker decides by the policy. That is the ONE
/// place a supervisor key may land in the free-text field. `Text = 0` is the
/// recommended path above, and nothing of this section is reachable there:
/// its reachable space is today's, state for state (pinned by the Tier-0
/// tests).
///
/// * **Each tab runs its own route** (`fallback`). A tab the text is
///   estimated not to fit, one with no free-text row (the preview form),
///   and every tab of a window that cannot fence a typed answer are
///   answered with their RECOMMENDED option instead — keyed as the
///   recommended path keys it, its chosen row (`focus` 0) that option, no
///   text ever written there (`HarnessType` needs a read that showed the tab
///   not falling back). A dialog may mix such tabs with typed ones.
///   `FitLostOnIt` / `FitLostOffIt` turn a tab to its recommended option,
///   the focus on it or off it: before its first read or after, while the
///   loop has written no text on it and no key of its is unread — never
///   back, as the loop latches a tab it decided so (`fell_back`). A
///   redraw that changes the route under a key still in flight is outside
///   the model, as every redraw not caused by a key is (a pane resized
///   under an unread key).
/// * **The queue code** `4 + Tabs + t` is the text decided on tab `t` —
///   one `send`, one queue item (`HarnessType`). Its consume is
///   `TextLands` (read whole), `TextPartly` (its head dropped by the
///   dialog's typeahead window, O4, leaving a suffix) or `KeyRefused`
///   (dropped whole, or read after the dialog closed: into the composer).
/// * **No retry, and once per tab.** A text the box did not take is handed
///   over, never written again: a second text behind an unread first would
///   land in a field no longer empty. `HarnessType` has no retry clause on
///   the box as keyed — it needs `mine` 0, a read that showed the box
///   CHANGED since its last key (which a person's move does as well as the
///   key's own effect) — and it needs `wrote` 0: the loop remembers the
///   text it wrote on this tab (the memory `OwnTextRemembered` names; the
///   real loop's `QuestionMemory::typed`, by which it hands a field
///   emptied again over as the text "gone"), so no text follows a first on
///   one tab whatever changed after it — the law `TextNeverWrittenTwice`,
///   refuted without the memory and by the fence alone. `HarnessType` also
///   sets `retried` to `MaxRetry` — no try left on the box as keyed, as the
///   real loop marks a text's pending write (main's P3 tries a KEY again
///   after the back-off; a text never); on the box as keyed (the field
///   focused, empty, not own) neither a move nor an Enter is enabled
///   anyway, so that is the loop's record, not a guard the proof leans on
///   (measured: at `retried = 0` every law still holds).
/// * **`own`** is the field holding EXACTLY the supervisor's text, typed by
///   it into that tab's empty, focused field (`TextLands` clean). Anything
///   else in the field is `typed` — a person's text, their keys added to
///   the supervisor's, a head-dropped suffix, a text landed on a field
///   that was not empty — and hands the question over as a person's text
///   does today (R2 d). Of those, `exact` is a person's own copy of the
///   policy text (`PersonTypesPolicyText`) on a tab the supervisor wrote no
///   text on: the one text no read of the field tells from the
///   supervisor's — only its memory does. (Typed on a tab it DID write its
///   text on, the field holds exactly its text again: `own`, by every read
///   and by its memory, as the real ownership rule has it.) On a text tab the chosen row (`focus` 0) is the
///   free-text row until own text is there; on a multi-select (`Multi =
///   1`) the typed row then ticks itself and the `Submit`/`Next` button
///   becomes the chosen row, with the focus still in the field (c3-05,
///   c4-09 on 2.1.283: `[✔]` and the chip `☒` as the text lands).
/// * **The laws, as they read here.** `NoDigitInTheField`: an Enter on a
///   question tab that does not fall back is in the field unless the field
///   holds exactly own text (over a person's text it would submit their
///   words as the policy's, over an empty field it answers nothing), so
///   `HarnessEnter` on such a tab needs a read that showed own text
///   (`seen_own`); on a tab that falls back it is any row but its
///   recommended option, as on the recommended path.
///   `OwnTextOnlyIntoItsEmptyField` (`astray`): the text is read only on
///   the tab it was decided on, into its focused, empty field — decided on
///   a read that showed exactly that, and written under the same two
///   guards as every key.
/// * **The multi-select Enter hazard** (c3c, measured on 2.1.283): Enter
///   while the focus is in a multi-select's field UNTICKS the typed row
///   (`[✔]` to `[ ]`, the chip `☒` to `☐`), and the dialog then answers
///   "did not answer". So on a multi-select the loop moves off the field
///   (`↓` onto the button, `HarnessMove`) and presses Enter on the button
///   only. `EnterWhereTyped = 1` (an Enter wherever own text was read) is
///   that hazard as a dial, committed 0: at 1 it breaks
///   `NoDigitInTheField` with no person acting at all.
/// * **Growth upward and reflow.** The text wraps the field over more rows
///   (c1-04: one row to three), and a dialog at the pane's foot grows
///   UPWARD, moving the field's row (c3-04 to c3-05: two rows up); on a
///   multi-select `↓` reflows the text by a cell or two (c3-06) without
///   changing its one-space join. The model's box is its tab, focus row,
///   focus parity, text and ownership, not its geometry: the real loop
///   guards each key on the row as drawn on the read it was decided on, a
///   changed drawing reads as a change (`fresh` 0), and a dialog cut at its
///   head reads Unknown and is handed over.
///
/// A second ASSUMPTION at `Text = 1`, `OwnTextRemembered` (committed 1): the
/// loop remembers the text it typed, per question, so a person's copy of
/// the policy text is never taken for its own, and no text is written on a
/// tab twice. At 0 (no memory: a person's exact copy reads as the loop's
/// own, and a written text is forgotten) `NoDigitInTheField` is refuted —
/// through a person's copy and through nothing else: a suffix, the text
/// plus more and a person's own words are told apart by the field's
/// byte-exact read, not by the memory — and so is `TextNeverWrittenTwice`.
/// A lost memory (a restart, a claim handover) is therefore a stall, never
/// an answer.
///
/// Out of the model for the text, and owed to the tests of the code that
/// types it: that the one `send` is one ordered queue item (its bytes
/// written under one lock hold, after any key queued before it); that the
/// text carries no digit and no newline (no byte of it selects an option
/// or submits); that the reader's one-space join of its wrapped rows gives
/// it back exactly; the in-lock residual of `if-human=` (`PersonFence`
/// above); the fit estimate itself (which tab falls back is the decider's,
/// `text_fits`; the model covers every choice of it); and a dialog that
/// mixes a single-select tab with a multi-select one (c4): `Multi` is one
/// constant for the whole dialog, so such a dialog is bound tab form by
/// tab form, not as one run — its typed multi-select tab and a typed
/// single-select tab each fit a model instance, a multi-select tab that
/// falls back fits both. Tier-1 of `Text = 1`: the decider
/// (`policy/question_text_tests.rs`,
/// `tier1_text_mode_decider_chooses_only_what_the_model_enables`, over
/// typed and falling-back tabs; a blind, a memory-trusting and a
/// multi-as-single decider as its negative controls) and the loop that
/// types it (`supervise/run_question_text_tests.rs`,
/// `tier1_the_policy_text_goes_only_where_the_model_allows`: each `send` its
/// `HarnessType`, enabled where it goes, a tab too short for the text and
/// a dialog of a typed tab and a falling-back tab bound too; a loop whose
/// memory is seeded over a person's own copy of the text as its negative
/// control, caught at the Enter the model forbids; and
/// `a_persons_key_between_the_read_and_the_send_skips_the_policy_text`: a
/// `send` the person fence skipped is the model's `PersonKey`, after which
/// `HarnessType` is not enabled — enabled there at `PersonFence = 0`).
///
/// **The two instances** (the review of the question-lean re-port): a law
/// no member of the `Buggy = 1` family can break at a model's committed
/// constants is a GHOST to the workspace's non-vacuity sweep, and the two
/// text laws are unbreakable at `Text = 0`, where no text is written. So
/// this machine is checked as two models, each with only the laws its
/// constants can break: this one, the policy text (`answer-long-term@v1`,
/// `answer-short-term@v1`: `Text = 1`, a single-select dialog committed,
/// `Multi` its dial), and [`supervisor_question_answer_model`], the
/// recommended path, cut from it.
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
            // Their text in the free-text row: off the chosen row when the
            // chosen row is an option; with a policy text (`Text = 1`, a tab
            // not falling back) the free-text row IS the chosen row, and
            // their text — even keys added to the supervisor's own — makes
            // the field not its own.
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
            // Their text is EXACTLY the policy text (a person's copy of it):
            // the one text only the loop's memory tells from its own — no
            // read of the field can (`OwnTextRemembered`). On a tab the loop
            // wrote its text on (`wrote`), the field then holds exactly that
            // text again, whoever typed its bytes: it IS the loop's own by
            // every read and by its memory (the real ownership rule), and an
            // Enter on it submits the policy text the loop wrote. On any
            // other tab it is a person's copy (`exact`).
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
            // Enter answers the focused row: the tab moves on, the review
            // submits (or, on its cancel, rejects the tool call). With a
            // policy text, an Enter on a question tab that does not fall
            // back is in the field unless the field holds exactly the
            // supervisor's own text; on a tab that falls back the chosen
            // row is its recommended option, as on the recommended path.
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
            // The policy text read whole. Into its own tab's EMPTY field,
            // focused, it is the supervisor's own (`own`) — on a
            // multi-select the row ticks itself and the Submit/Next button
            // becomes the chosen row, the focus staying in the field.
            // Anywhere else it is astray, and it is text no one may take
            // for the supervisor's (`typed`).
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
            // The typeahead window dropped its head (O4): the field holds a
            // suffix of the text — not the supervisor's own.
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
            // THE TAB FALLS BACK (`Text = 1`): the text is estimated not to
            // fit its pane (or the tab has no free-text row, or the window
            // cannot fence a typed answer), so it is answered with its
            // recommended option — the chosen row is that option from here
            // on, the focus on it or off it. Only while the loop has written
            // no text on the tab and none of its keys is unread (a redraw
            // under a key in flight is outside the model, as any redraw is);
            // never back (the loop latches it).
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
            // A question tab drawn with its focus off the chosen row (the
            // vendor's option 1, the recommendation option 2; a toggled
            // option, the next choice another row) — before any read of it.
            action OpensOffChoice when (opened == 1 && tab <= Tabs - 1) {
                focus = 1;
                row = 1 - row;
                fresh = 0;
            }
            // A key of the supervisor's the dialog does not take: dropped
            // inside its typeahead window (O4), no effect — or, read after
            // the dialog closed, typed into the composer (`↑` recalls
            // history into it: `NoKeyIntoTheComposer`). ONE action, as
            // `EnterTaken` sets `in_field`: that consume is reached only past
            // the guards (`Buggy = 1`), so as an action of its own it was
            // dead in the committed model — which `ty check
            // --strict-vacuity` takes for a vacuous model unless the dead
            // action alone is the mutant, and here the mutant is the guards'.
            // (A key read after the close was dropped, or not, only in that
            // same uncommitted space; the composer is where it goes.)
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
            // Enter on the chosen row. With a policy text, on a question
            // tab that does not fall back only once the read showed the
            // field holding the supervisor's own text: on that field, or on
            // a multi-select's button after it. With no memory
            // (`OwnTextRemembered = 0`) a person's exact copy of the text
            // reads as its own.
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
            // The policy text, written into the focused free-text row the
            // read showed empty, on a tab that does not fall back — with no
            // retry clause on the box as keyed (`retried` is the loop's
            // record of it, THE POLICY TEXT), and once per tab whatever
            // changes after it: the loop remembers the text it wrote
            // (`wrote`, the memory `OwnTextRemembered` names) — under the
            // person fence (`touched`), as every key.
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

/// The actions only the policy text enables: each guard needs `Text == 1`
/// (`HarnessType`, `FitLostOnIt`, `FitLostOffIt`, `PersonTypesPolicyText`)
/// or a text's queue code, which only `HarnessType` writes (`TextLands`,
/// `TextPartly`) — false in every state at `Text = 0`.
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

/// THE DECLINE'S KEYSTROKES (aterm-agent `supervise/press.rs`,
/// `Session::press_decline`; the review's F1 and F3): a Bash
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
/// and would prove nothing):
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

/// THE RATING CARD'S `0` (aterm-agent `supervise/run.rs`, `Session::survey`, and its
/// busy-read twin in `await_turn_from`): the harness writes a guarded `0` at a
/// Claude Code rating card only where that `0` is taken and lands nowhere else, and
/// removes a `0` the card dropped only when the card can no longer take it — the property
/// the owner asked for: the person typing into the composer never receives a
/// `0` meant for a survey, and the harness never deletes its own dismissal.
///
/// `card` is the card on the screen (0 none, 1 open and YOUNG — inside the vendor's
/// 600 ms mount settle, which drops a digit and leaves it in the composer as text —
/// 2 open and aged); `hand` a person's keystroke within `human_grace_s`; `draft` text
/// typed in the composer; `boxed` an approval box up. `zero` is the `0` the harness wrote:
/// 0 none, 1 PENDING (written, and the vendor's 400 ms debounce may still take it: a
/// digit typed at an aged card is taken when the timer runs and ANY change to the composer
/// before that cancels the take, a backspace included), 2 STANDING (the debounce has run
/// and the composer still holds it: the card dropped it). `takes` is 1 while a pending `0`
/// is going to be taken (written at an aged card, which has not left since); `owed` is the
/// loop's memory that a `0` of its own may still be in the composer.
///
/// A press is enabled only for an AGED card with nobody's hand on the session, no draft, no
/// box and no `0` already of ours (a second would make `00`, which abandons the card and
/// stays in the composer); the stray `0` a card dropped is backspaced only when it is
/// STANDING — past the landing time, longer than the vendor's debounce — and is the
/// composer's whole text with no person at the keyboard, and a backspace the server
/// REFUSES (`halted`, `busy`, `rate`) leaves it owed. `Buggy = 1` presses whatever the state
/// (inside the settle, under a person's hand, over a draft or a box, on a `0` already
/// there), backspaces a pending `0` (the first prototype's flaw: it read the `0` still
/// pending at 500 ms and deleted its own dismissal), and forgets a `0` whose backspace was
/// refused — each caught by its invariant on its own path. Tier-1 (aterm-agent
/// `run_engine_tests.rs`) drives the real `Session` over the scripted server and projects
/// every `key` request onto this model.
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

/// THE USAGE WALL RESUMES (aterm-agent `supervise/policy/turn_end.rs`, the wall
/// arm of `decide_turn_end`, over the reader's wall): a session at a usage wall
/// is continued once its tokens are back — automatically, in bounded time,
/// on a ladder that never gives up.
///
/// `wall` 0 the worker works, 1 the wall shows (a point the loop decides),
/// 2 an act is in flight, 3 the wall is MASKED (drawn, but read as busy: a
/// weekly wall behind a background shell), 4 the
/// loop has given up, 5 the wall's rows are OUT OF VIEW (scrolled off, a `/model`
/// under them: the tab reads busy on its shell) and `mem` says the loop still
/// remembers the wall it waits on, 6 the wall's notice LEFT AN IDLE SCREEN (Esc
/// on the vendor's own countdown, a person's local command on a tab with no
/// shell: idle, no wall) and the loop waits on the reset the notice named —
/// remembered across a loop's restart by its open ledger row (`mem` again).
/// `ran` marks a person's turn served at a left wall that the loop did not
/// see, the wait left standing over it. `other` 1 the left wall's screen
/// shows ANOTHER conversation's history (a person `/resume`d it): an older
/// limit's notice with turns served under it, this limit standing. `bg` the agent's own background work. `clock` the ticks
/// since the wall appeared (saturating); the account has its tokens back at
/// `due` (`ResetAt`, or `Skew` ticks later). `view` 0 the reset is placed where the notice
/// says, 1 placed in the past (a bare clock time read by the session-window
/// rule). `tries` the acts typed at this wall (saturating), `waited` the ticks
/// since the last one, `blind` the acts typed at this wall before the tokens
/// came (saturating), `sitting` 1 for a wall FOUND standing with nothing on
/// the screen to date its print.
///
/// A wall found sitting is placed by the session window's rule: a bare clock
/// more than five hours ahead reads as passed (`view = 1`), and its one try is
/// the price the policy accepts (`NoBlindAct` allows exactly that one). The
/// wall that ANSWERS a try (`Met`: printed again after a turn, or standing
/// with the line held and no turn run — Claude Code queues a line typed into a
/// standing limit) is a print the loop watched, and the reset is placed where
/// the notice says (`view = 0`); `Buggy` keeps the guess, and the ladder types
/// into the standing wall every rung until the reset (24 lines in one
/// incident), caught by `NoBlindAct`. Every act waits for the reset as
/// placed AND its rung (the real `max(reset + grace, since + rung)`).
///
/// A notice that leaves an idle screen (`NoticeLeaves`, the `Leaves` dial) is
/// waited on to the same reset, and acted on there as the wall that shows is
/// (`Buggy`: `LeftReadAsTurnEnd`, the left limit decided as a turn's end — the
/// continuation typed into the standing limit at once, caught by `NoBlindAct`).
/// A person's turn there that the account serves (`PersonTurnServed`, its
/// tokens back — at the named reset or before it, `LimitLiftedEarly`: a plan
/// changed, usage bought) is work since the wall, and ends the wait however
/// short it was (`Buggy`: `PersonTurnUnseen`, a turn no read saw busy read as
/// no turn — the person's continuation held to the reset, caught by
/// `NoWaitOverAServedTurn`). Another conversation's history on the screen
/// (`ConversationResumed`) is no turn of this limit's, served or not
/// (`Buggy`: `OtherLimitsTurnRead`, its older limit's served turn read as one
/// since this limit — the wait ended and the continuation typed into the
/// standing limit, caught by `NoBlindAct`; the review of 2026-10-14 measured
/// it on 2.1.300). So is one `/resume`d while the wall SHOWS, its countdown
/// armed (`ArmedConversationResumed`, `other` 2: W1 of ATERM-KI-1015-1):
/// Claude Code drops the countdown and redraws an older limit the vendor
/// continued by itself, whose word (`Usage limit reset · continuing
/// automatically`) is no continuation of this limit's (`Buggy`:
/// `OtherLimitsContinuationRead`, that word tied to no limit — the wait ended
/// and the continue policy typed into the standing limit, caught by
/// `NoBlindAct`; measured on 2.1.300, 2026-10-15: the stub's 429 22 minutes
/// before the reset).
///
/// `held` 1: an act of the policy's OWN is inside its take window — typed, no
/// read has seen it taken, and its point not judged yet (30 s in the real
/// policy, under a tick: the next `Tick` ends it, as does a turn the account
/// serves, a point with work). Both of its acts have one (the `OwnAct` dial):
/// the continue policy's at a turn's end (`OwnActTyped`), and the wall's own
/// retry (`Act`). No act is decided while it is (`Act` waits for the window).
/// A wall that answers the act at once (the stub's 429, no busy read: a
/// `WallAppears` after `OwnActTyped`, a `MetInWindow` after `Act`) is read
/// with `held` still 1, and may leave the screen inside the window
/// (`NoticeLeaves`). The wall is the loop's from its first reading all the
/// same; `Buggy`: `TakeWindowDropsWall`, the wall folded in only at a point,
/// so its notice gone by the window's end reads as the act's short turn and
/// the continue policy types into the standing limit (round 15's run on
/// 2.1.300, 2026-10-14), caught by `NoBlindAct`; and `RetryReadAsTaken`, the
/// wall's answer to a retry read inside its window but not kept as the
/// retry's answer, so the retry, its answer's notice gone by the window's
/// end, reads as taken and the wall is forgotten — the continue policy then
/// typed into the standing limit (round 16's review), caught by
/// `WallRemembered`.
///
/// `unread` 1: the wall's notice LEFT BEFORE ANY READ after the act it
/// answered — the limit answered at once and a person's Esc on the vendor's
/// countdown came before the loop's first read (round 16's residual (a), the
/// lab's `tw16after` on 2.1.300): `WallAppearsLeft` after `OwnActTyped`,
/// `MetLeftUnread` after `Act` (the `Unread` dial: 0 switches both off). The
/// screen the loop reads shows only the
/// vendor's cancel under the notice (`Automatic continue cancelled`), no
/// wall; that notice is the act's answer all the same (the real
/// `TurnEndState::left_answer`: a notice the act was not typed over, the
/// cancel right under it, nothing served since), and the limit is waited on
/// to the reset it named. `Buggy`: `UnreadLeftDropped`, the cancel read as
/// no limit, so the act's window ends on its short turn and the continue
/// policy types into the standing limit (measured on 1afcdade3), caught by
/// `NoBlindAct`; and `UnreadAnswerReadAsTaken`, a retry's answer gone before
/// any read, the retry read as taken and the wall forgotten, caught by
/// `WallRemembered`. The next tick (the window's end) clears it: past the
/// window the left wall is the track's, as any left wall is.
///
/// A PERSON'S turn answered the same way (`PersonWallLeft`, round 17's
/// residual (a)): no act of the loop's is in flight; a person's prompt at a
/// turn's end meets the limit at once and their Esc on the countdown comes
/// before the loop's next read. The notice is new since the loop last
/// looked, and it is that turn's answer all the same (the real
/// `TurnEndState::left_answer`, confirmed by a transcript row written since
/// that look where there is one): waited on to the reset it named. Read as
/// no limit, the continue policy's next act goes into it —
/// `UnreadLeftDropped` again (measured on 745d63b36, the lab's `pc18b1`), caught by
/// `NoBlindAct`.
///
/// ANOTHER CONVERSATION'S OLDER CANCELLED LIMIT inside the own act's window
/// (`OlderLeftResumed`, round 17's residual (a)): a person `/resume`s, before
/// the loop's first read after its act, a conversation whose last thing is
/// a limit cancelled on its countdown long ago. Nothing stands (`due` now),
/// and with no transcript nothing tells that notice from the act's own
/// answer: the loop reads it as the answer, and nothing dates its print
/// (`sitting`), so its bare clock is placed never more than a session window
/// out — a reset already past is passed (`view = 1`), the one try the
/// policy accepts. `Buggy`: `OlderLeftPlacedTomorrow`, placed on the print
/// of the act it was taken to answer, the past clock read as TOMORROW's
/// (`view = 2`: no act for a day while the session is free; measured on
/// 745d63b36), caught by `NoDayWait` and the liveness claim.
///
/// USAGE CREDITS (the `Credits` dial, accounts v2 S5): a turn that crosses
/// the limit is carried by credits and served, no wall drawn
/// (`CreditsServes`, `credits` 1; lab E4). `Buggy`: `CreditsReadAsWall`,
/// credits taken for a spend wall, caught by `NoWallOnCredits`.
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
            // A wall FOUND standing (a loop that starts at it, a relaunch that
            // redraws the conversation without its done row): nothing dates
            // its print, the session rule places its bare clock, and a clock
            // more than five hours ahead reads as passed.
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
            // The wall's notice LEAVES AN IDLE SCREEN (the `Leaves` dial: 0
            // switches it off): Esc on the vendor's own countdown (`Automatic
            // continue cancelled`), a person's `/usage` or `/model` under it on
            // a tab with no shell. The screen reads idle, no wall, no work.
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
            // The account gets its tokens back BEFORE the reset the notice
            // named (a plan changed, usage bought; the lab's `mode ok`): a
            // turn served now is served before the reset.
            action LimitLiftedEarly when (wall == 6 && clock <= due - 1) {
                due = clock;
            }
            // A person `/resume`s ANOTHER conversation at the left wall: the
            // screen redraws its history — an older limit's notice, lifted
            // long ago, with turns served under it — and the host hands over
            // its transcript. Nothing of this limit's.
            action ConversationResumed when (Leaves == 1 && wall == 6 && other == 0) {
                other = 1;
            }
            // A person `/resume`s ANOTHER conversation while the wall SHOWS,
            // the vendor's countdown armed (W1 of ATERM-KI-1015-1, 2.1.300):
            // Claude Code drops the countdown and redraws that conversation's
            // history — an older limit and the vendor's own continuation of
            // it (`Usage limit reset · continuing automatically`), no `❯` row
            // since. The screen reads idle with no wall; this limit stands.
            action ArmedConversationResumed when (
                Leaves == 1 && wall == 1 && bg == 0 && other == 0
            ) {
                wall = 6;
                other = 2;
            }
            // The limit answers the own act at once AND a person's Esc on the
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

            // A PERSON'S prompt at a turn's end, no act of the loop's in
            // flight, answered at once by the limit, AND their Esc on the
            // vendor's countdown before the loop's next read: the screen it
            // reads shows only the cancel under the notice under the person's
            // prompt, no wall.
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
            // Inside the own act's window, before the loop's first read
            // after it, a person `/resume`s a conversation whose last thing
            // is an OLDER limit cancelled on its countdown: nothing stands
            // (the tokens are here now), the screen shows a cancel under a
            // notice, and nothing dates that notice's print. Read as the
            // act's answer, its bare clock is placed never more than a
            // session window out: a reset already past is passed.
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
            // it is served, at 100%, and no wall is drawn; at its end the
            // account is as it was, the next turn's wall the world's again.
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
            // The first act: `Grace` past the reset the notice names (150 s and a
            // session's share of a minute in the real policy); each
            // one after a wall that answered again: a rung, the last for every
            // one after it — for ever. Its take window opens with it.
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
            // ... the wall answers again: the next rung counts from THIS
            // appearance (the real `since`), as a fresh try's answer is
            // immediate but time may pass while it is in flight. The answer is
            // a print the loop watched: the reset is placed where the notice
            // says. A turn ran into it (its point judges the act at once), or
            // the act's window had passed.
            action Met when (wall == 2 && clock <= due - 1) {
                wall = 1;
                waited = 0;
                view = 0;
                held = 0;
            }
            // ... or answers it at once, no turn run (the stub's 429), INSIDE
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
            // The wall drawn behind background work, read as busy: no point. It
            // stays masked (the reader lifts it when the shell ends, which the
            // world never has to do, so no lift is modelled: a defect action
            // must be caught alone, and a lift alone could not even fire).
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
            // read as a turn served since THIS limit (evidence tied to no
            // limit): the wait ends while the tokens are not back, and the
            // continue policy types into the standing limit.
            action OtherLimitsTurnRead when (Buggy == 1 && wall == 6 && other == 1 && clock <= due - 1) {
                blind = if blind <= 1 { blind + 1 } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                unread = 0;
            }
            // Another conversation's older limit continued by the vendor,
            // its word read as THIS limit's continuation (evidence tied to no
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
            // The wall answers the act, and the loop keeps its guess: a wall
            // standing after an act nobody took read as no print,
            // the ladder typing into it every rung until the reset.
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

/// THE USAGE WALL ENDS ([`supervisor_usage_wall_model`]): `[]<>(the worker works \/ the
/// tokens are not back yet)` — on every fair behaviour, a wall whose tokens
/// have come does not stand for good. Fairness assumed, and nothing more: the
/// clock ticks while the Mac is awake (`Tick`), the loop acts once an act is
/// enabled (`Act`), and the account answers a continuation once it has its
/// tokens (`Taken`). The world is NOT fair: background work may start and
/// never end, and a wall may come late. Broken alone by `WallMasked` (a
/// wall drawn behind background work, read as busy, so no
/// point ever forms), by `GivesUp` (a ladder with a last try), by
/// `MemoryLost` and by `OlderLeftPlacedTomorrow` (an older cancelled limit
/// nothing dated, its past clock placed a day out: no act while the tokens
/// are here).
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

/// THE MODEL SWITCH (aterm-agent `supervise/policy/turn_end.rs`, the model
/// bucket's arm of `decide_turn_end` and `TurnEndState::restarted`, and the
/// host's `restart` in `supervise/turn_end_loop.rs`): a tab at a model
/// limit is moved to the fallback model by a relaunch the host makes when the
/// agent's own jobs allow it, and comes back to the bucket's model only at a
/// reset the notice NAMED and that was still ahead when the switch was made
/// — never on a guess.
///
/// `bucket` 1 the model's limit stands; `named` 1 the notice names a reset
/// (0: an entitlement, `requires usage credits`); `on` 1 the tab is on the
/// fallback; `back` 1 the switch owes a return at the reset
/// (`ModelSwitch::back_at`); `jobs` 1 the agent's own background work runs
/// (the host refuses the relaunch while it does); `clock` the ticks since the
/// wall appeared; `probed` the wall has been acted on (the continuation typed
/// at the reset goes FIRST, before the fallback is asked for again, and a
/// made switch is an act too); `tries` the asks the host refused; `flips` the
/// relaunches since the worker last did real work; `gaveup`, `held`, `badret`
/// are the defects' marks. One tick is one minute.
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
            // At the reset the CONTINUATION goes first: the wall may have
            // lifted, and a switch made at that moment would be for nothing.
            action Continue when (bucket == 1 && on == 0 && named == 1 && ResetAt <= clock && probed == 0) {
                probed = 1;
            }
            // Ask the host for the fallback. It is made when the agent's jobs
            // are not running and refused (`wait:changed`) while they are; the
            // ask is repeated on the restart back-off, never given up. A
            // switch made owes a return only at a named reset still ahead.
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
            // make now is DEFERRED and holds nothing.
            action Return when (on == 1 && back == 1 && ResetAt <= clock && jobs == 0) {
                on = 0;
                back = 0;
                flips = if flips <= 3 { flips + 1 } else { flips };
            }

            // ---- the defects (dead at the committed config) ----
            // A return with none owed (`Guess` ticks after the switch: the 240
            // relaunches in five days, 2026-09-22 to 09-27): back to a wall
            // that stands, and the fallback asked for again.
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

/// THE FALLBACK IS MADE ([`supervisor_model_switch_model`]): `[]<>(the tab is
/// on the fallback \/ the limit lifted \/ the agent's jobs run)` — on every
/// fair behaviour, a tab whose jobs are not holding the relaunch is moved.
/// The jobs are the environment's and are NOT fair: they may run for ever, and
/// then the tab stays where it is, asked again at every back-off ("if they
/// end, it is made"). Fairness assumed: the loop asks (`AskFallback`) and, at a
/// reset, types the continuation first (`Continue`). Broken alone by
/// `GivesUp` (the asks stop after a few refusals).
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

/// THE SAME BOX IS ONE POINT (aterm-agent `supervise/run.rs`: the review
/// key of a box, `point_key`, and the look that hands a point over; FIX 3:
/// a re-alert, `CLEARED` then `ESCALATED` 7 s apart for the same box): a
/// request standing on the screen is asked
/// of the manager ONCE however the vendor redraws it — re-wrapped at another
/// width, its focus moved, its auto-deny countdown ticking, the transcript
/// reflowed above it — and EVERY request the loop looks at is asked, a
/// second box drawn over the first with no read between included (two Edit
/// boxes on one path, their diffs different).
///
/// `req` is the request on the screen (0 none; 1 and 2 share a head — the
/// same kind and path, the review's (a); 3 is request 2 asked AGAIN, which
/// the worker does only after it worked); `seen` how many requests were
/// drawn; `form` the drawing of the one up (a redraw flips it); `key` the
/// loop's review key of what it reads; `handed` the key of the point last
/// handed over; `live` that a read since the last look saw the worker's own
/// busy row (a turn ran); `asks` the asks of the request up (counted to 2:
/// one past the bound `OneAskPerRequest` states, which is all a slip needs
/// to break it, so the mutants' space stays finite); `looked` that the loop
/// looked at it. `Look` is the loop's look: a point already handed, with no
/// live busy read since, is passed over; anything else is a new point,
/// asked.
///
/// `Buggy = 1` is the three slips this model must catch, together, each
/// breaking an invariant of its own — and each has its own dial as well, so
/// a test can turn one on alone: `Drawing` keys the DRAWING (the rows as
/// laid out, the countdown, the rows above — the key before FIX 3: a redraw
/// asks one request twice, `OneAskPerRequest`); `Coarse` keys the head
/// alone (kind and path — the rejected plan: the second of two Edit boxes on
/// one path is looked at and never asked, `EveryLookedRequestIsAsked`);
/// `WritingWorks` takes a read that shows only output being written
/// (`writing`: no composer, no box, no busy row, the screen never held
/// still) for the worker's turn (the review's (c): the box redrawn through
/// it is asked twice, `OneAskPerRequest`).
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
/// the wait makes under a box's row, and each real ask posted right after
/// the read the model asks at — with the key before FIX 3 replayed through
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

            // The next request drawn — over the last with no read between,
            // or after it left; the SAME request again (3) only after work.
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
            // The vendor redraws the request up: re-wrapped, its focus moved,
            // its countdown ticked, the transcript reflowed above it.
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
