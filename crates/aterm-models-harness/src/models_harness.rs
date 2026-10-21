// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded models for the aterm wrapper's harness core
//! (`docs/DESIGN-aterm-wrapper-2026-09-17.md` §11 item 7): the bounded child runner's worker
//! lifecycle (`align::capture_bounded`), the Claude Code upgrade ladder and its
//! notice/drain/limit/work-end laws, the login wall, the usage tap, and the model
//! priority/ladder/switch, all in `aterm_agent::harness`. Each is a hand-written description of
//! that code; only a Tier-1 bind in the code's own tests makes it a statement about the program.

use aterm_spec::derive::*;

/// A bounded harness capture may time out and reap its direct child while an escaped descendant
/// still holds stdout open; the capture returns only after the stdout reader has finished or been
/// cancelled and joined. Tier-1 runs the real `align::capture_bounded` with an escaped
/// descendant and observes the reader's completion at return. `Buggy=1` returns on child exit
/// alone, leaking the reader past the caller's deadline.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_capture_worker_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        HarnessCaptureWorkerLifecycle {
            const Buggy = 0;
            var child_reaped = 0;
            var reader_done = 0;
            var deadline = 0;
            var returned = 0;

            action ChildExit when (child_reaped == 0 && returned == 0) {
                child_reaped = 1;
            }
            // The direct child may have exited while an escaped descendant still holds a pipe;
            // its worker deadline still applies.
            action Deadline when (deadline == 0 && returned == 0) {
                deadline = 1;
            }
            action TimeoutKill when (child_reaped == 0 && returned == 0) {
                deadline = 1;
                child_reaped = 1;
            }
            action ReaderFinish when (reader_done == 0 && returned == 0) {
                reader_done = 1;
            }
            action Return when (
                returned == 0 && child_reaped == 1 && (reader_done == 1 || Buggy == 1)
            ) {
                returned = 1;
            }

            invariant NoReturnBeforeWorkersJoin:
                returned == 0 || (child_reaped == 1 && reader_done == 1);
        }
    }
}

/// The live upgrade's ladder: prefer a natural pause, and the longer a session has been behind
/// the less it waits. After 20 min a pause that only looks quiet counts, after 1 h "you're at the
/// tab" means only a keystroke in the last 20 s, and by about 2 h it moves at the first such
/// pause; it never moves over a draft, a dialog or a running turn.
/// `aterm_agent::harness::upgrade::gate` and `upgrade::rung`, and `upgrade_drive::St::still`, for
/// a Claude Code session (its move is the restart the quiet flow takes).
///
/// The lane. `rung` (0 Prefer, 1 Settled, 2 KeysOnly, 3 Land) climbs with the clock (`Advance`)
/// while a move is owed (`behind`). `Move` never fires while the session's turn runs (`work`), a
/// draft stands (`draft`) or a box is up (`dialog`); a person holds it (anyone near the tab,
/// `keys` 1 within `human_grace_s`, until KeysOnly, then only a keystroke within 20 s, `keys` 2);
/// and it waits for a settle: the screen's own quiet (`quiet`, which a `Repaint` ends), or from
/// Settled the reader's run of idle looks at the same last words (`seen` 2), and at Land none. A
/// turn draws on the screen (a status row, streaming words), so it restarts the run of looks.
///
/// The environment: turns start and end (`TurnStarts`, `TurnEnds`), repaints, a person types,
/// pauses and leaves, drafts and boxes come and go.
///
/// Safety, each with its `Buggy` member, each dead action caught alone
/// (`verify::audit_dead_negative_controls`): `NeverOverADraft` (`MoveOverADraft`),
/// `NeverOverABox` (`MoveOverABox`), `NeverWithinKeysGap` (`NowWaivesThePerson`: a `--now` that
/// waives the person outright), `NoRunningWorkInterrupted` (`MoveMidTurn`), with the moves
/// recording what stood (`at_*`), and `QuietPauseCounts` (`SeqSettle`: a look at a pause the
/// ladder counts from Settled never waits `settling` for a screen that only repainted). The
/// livelock below is also a bad state the moment its mutant decides it, so no mutant is caught by
/// the liveness alone: `TheGraceNarrowsAtKeysOnly` (`AttendedWithoutLadder`: a pause from
/// KeysOnly on waited on for a person merely near the tab). Each mutant fires only from a state
/// no mutant has touched (`slipped`), one slip per behaviour.
///
/// The Codex lane's daemon pin, goal footer, second input mark and another session's turn were
/// this machine's until that lane was retired (2026-10-16): a codex-alab's restart is carried by
/// its own relaunch, never moved live.
///
/// Liveness: [`harness_upgrade_ladder_liveness`].
///
/// Tier-1 in aterm-agent: `upgrade_ladder_claude_tests.rs`, over every reachable state against
/// Claude Code's look and step, catching every `Buggy` member.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_ladder_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeLadder {
            const Buggy = 0;
            const LandRung = 3;
            var rung = 0;
            var behind = 1;
            var work = 0;
            var quiet = 0;
            var seen = 0;
            var keys = 0;
            var draft = 0;
            var dialog = 0;
            var narrows = 1;
            var moved = 0;
            var missed = 0;
            var at_draft = 0;
            var at_box = 0;
            var at_keys = 0;
            var at_work = 0;
            var slipped = 0;

            // The wall clock, while a move is owed: the ladder's rung.
            action Advance when (behind == 1 && rung <= LandRung - 1) {
                rung = rung + 1;
            }
            // A turn of the session's own; not guarded on `behind`, as a turn runs on after a
            // move. It draws (a status row, streaming words), restarting the still run.
            action TurnStarts when (work == 0) {
                work = 1;
                quiet = 0;
                seen = 0;
            }
            action TurnEnds when (work == 1) {
                work = 0;
            }
            // A redraw with no new words (a footer clock).
            action Repaint when (behind == 1) {
                quiet = 0;
            }
            action Still when (behind == 1 && work == 0 && quiet == 0) {
                quiet = 1;
            }
            // A look at an idle screen: the run of looks at the same last words grows.
            action Look when (behind == 1 && work == 0 && seen <= 1) {
                seen = seen + 1;
            }
            action Type when (behind == 1) {
                keys = 2;
            }
            action KeysPause when (behind == 1 && keys == 2) {
                keys = 1;
            }
            action KeysAway when (behind == 1 && keys == 1) {
                keys = 0;
            }
            action DraftTyped when (behind == 1 && draft == 0) {
                draft = 1;
            }
            action DraftCleared when (behind == 1 && draft == 1) {
                draft = 0;
            }
            action BoxUp when (behind == 1 && dialog == 0) {
                dialog = 1;
            }
            action BoxDown when (behind == 1 && dialog == 1) {
                dialog = 0;
            }
            // The step: no turn running, no draft, no box, the person's floor, and a settle.
            action Move when (
                behind == 1 && work == 0 && draft == 0 && dialog == 0 &&
                (keys == 0 || (rung > 1 && narrows == 1 && keys <= 1)) &&
                (rung == LandRung || quiet == 1 || (rung > 0 && seen == 2))
            ) {
                behind = 0;
                moved = 1;
                at_draft = draft;
                at_box = dialog;
                at_keys = keys;
                at_work = work;
            }
            // The mutants, each its own dead action. A look at a pause the ladder counts that
            // waits `settling` because the screen repainted.
            action SeqSettle when (
                Buggy == 1 && slipped == 0 &&
                behind == 1 && work == 0 && draft == 0 && dialog == 0 &&
                (keys == 0 || (rung > 1 && narrows == 1 && keys <= 1)) &&
                rung > 0 && rung <= LandRung - 1 && seen == 2 && quiet == 0 && missed == 0
            ) {
                slipped = 1;
                missed = 1;
            }
            // A person near the tab holds it at every rung (`human_grace_s` never narrows), at
            // a pause from KeysOnly on where nothing else stands.
            action AttendedWithoutLadder when (
                Buggy == 1 && slipped == 0 &&
                narrows == 1 && behind == 1 && rung > 1 && keys == 1 &&
                work == 0 && draft == 0 && dialog == 0
            ) {
                slipped = 1;
                narrows = 0;
            }
            // A `--now` that waives the person outright.
            action NowWaivesThePerson when (
                Buggy == 1 && slipped == 0 &&
                behind == 1 && work == 0 && draft == 0 && dialog == 0 &&
                keys == 2 && rung == LandRung
            ) {
                slipped = 1;
                behind = 0;
                moved = 1;
                at_draft = draft;
                at_box = dialog;
                at_keys = keys;
                at_work = work;
            }
            action MoveOverADraft when (
                Buggy == 1 && slipped == 0 &&
                behind == 1 && work == 0 && draft == 1 && rung == LandRung
            ) {
                slipped = 1;
                behind = 0;
                moved = 1;
                at_draft = draft;
                at_box = dialog;
                at_keys = keys;
                at_work = work;
            }
            action MoveOverABox when (
                Buggy == 1 && slipped == 0 &&
                behind == 1 && work == 0 && dialog == 1 && rung == LandRung
            ) {
                slipped = 1;
                behind = 0;
                moved = 1;
                at_draft = draft;
                at_box = dialog;
                at_keys = keys;
                at_work = work;
            }
            // A session ended while its own turn runs.
            action MoveMidTurn when (
                Buggy == 1 && slipped == 0 &&
                behind == 1 && work == 1 && rung == LandRung
            ) {
                slipped = 1;
                behind = 0;
                moved = 1;
                at_draft = draft;
                at_box = dialog;
                at_keys = keys;
                at_work = work;
            }

            invariant NeverOverADraft: at_draft == 0;
            invariant NeverOverABox: at_box == 0;
            invariant NeverWithinKeysGap: at_keys <= 1;
            invariant NoRunningWorkInterrupted: at_work == 0;
            invariant QuietPauseCounts: missed == 0;
            invariant TheGraceNarrowsAtKeysOnly: narrows == 1;
        }
    }
}

/// The upgrade lands: the ladder's law as a property of whole behaviours (activity delays, it
/// never disables): `[]<>(moved \/ a-floor-stands)`. Again and again the move is done
/// (`behind = 0`); or, while the move is owed, a floor no rung relaxes stands: the session's
/// running turn (`work = 1`), a draft, a box or a keystroke within 20 s (`keys = 2`). Leaving it
/// for good means no floor ever again and still no move: the ladder waiting on comfort for ever.
///
/// The environment is not fair: turns, repaints, keystrokes, a person coming back, drafts and
/// boxes may go on for ever, which is why a floor that keeps standing satisfies the goal. The
/// verdict assumes only:
///
/// * `Advance`, weakly fair: the wall clock moves, up to Land, where no settle is waited for;
/// * `Move`, weakly fair: the look is the move wherever its gate stays open. With the Codex
///   lane's other session gone, nothing the world does re-opens a closed gate behind the lane's
///   back without a floor standing meanwhile, which the goal already counts, so weak fairness
///   is enough (strong was needed while another session's goal could idle between its turns).
///
/// The mutant that breaks it: `AttendedWithoutLadder` (a person back within `human_grace_s` for
/// ever holds the move for ever). A livelock: nothing is ever stuck, so no deadlock check sees
/// it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(harness_upgrade_ladder_model)]
pub fn harness_upgrade_ladder_liveness() -> Liveness {
    Liveness {
        name: "TheUpgradeLands",
        goal: or_(
            eq(var("behind"), int(0)),
            or_(
                or_(eq(var("work"), int(1)), eq(var("draft"), int(1))),
                or_(eq(var("dialog"), int(1)), eq(var("keys"), int(2))),
            ),
        ),
        weak: vec!["Advance", "Move"],
        strong: vec![],
        mutants: vec!["AttendedWithoutLadder"],
    }
}

/// A READY answer is usable only by the process and tab that received the notice, with a unique
/// live owner of that conversation and a complete session-file scan. The process-start token
/// stands for the kernel PID-reuse guard.
///
/// A notice whose process is gone holds nothing for ever. Without this, an announced upgrade
/// whose notified process exited without a relaunch waits on it for good once a person resumes
/// the conversation by hand (`wait:notice-owned-by-other-process` at every visit, `--now`
/// included). Once the kernel proves that process gone (its pid names no process, or another:
/// `gone`), the live process holding the conversation is asked afresh (`Reask`) in a new round:
/// the markers forgotten and the salt minted again, so the gone process's READY in the shared
/// transcript can never answer a notice typed to the new one. While the notified process lives,
/// a process it never reached still waits.
///
/// Processes: A (tab 1, pid 1, start 1) got the first notice. B (pid 2, start 2) resumes the
/// conversation in A's tab (`Resume`) or in tab 2 (`OtherTab`). A process the kernel started
/// later under A's pid (start 3) holds it in A's tab (`Recycled`). `pid == 0`: nobody holds the
/// conversation. `ready_pid`/`ready_start`: the process whose notice the transcript's READY
/// answers. `Look` is a visit with complete proofs (one live owner, a whole scan) that finds
/// nothing to do, derived from the reducer's guards (enabled exactly where neither `Reask` nor
/// `Terminate` is); ghost `stranded` records such a wait on a gone notice.
///
/// Properties: `OnlyIssuerSignaled`, `NoDuplicateOwnerSignal`, `NoPartialScanSignal`,
/// `OnlyOnItsOwnAnswer` (a restart acts only on a READY to a notice typed to the process it
/// signals) and `NeverWaitsOnAGoneNotice`. One knob per defect: `NoReask` never asks afresh (the
/// re-arm after a rest reaches only a stopped round); `KeepRound` asks afresh but keeps the round,
/// so the gone process's READY answers the new marker. `Buggy=1` is a conversation-keyed reducer
/// (accepting READY in another tab or after a partial scan) that never asks afresh. Not modelled:
/// the release a gone process was owed, which nothing can type to it
/// (`upgrade_drive::St::notice_gone` drops it and the ledger says so). Tier-1: aterm-agent's
/// `harness::upgrade_drive` tests, over the real visit.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_notice_owner_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeNoticeOwner {
            const Buggy = 0;
            const NoReask = 0;
            const KeepRound = 0;
            var phase = 0;
            var tab = 1;
            var pid = 1;
            var start = 1;
            var owner_tab = 0;
            var owner_pid = 0;
            var owner_start = 0;
            var owners = 1;
            var scan_complete = 1;
            var ready = 0;
            var ready_pid = 0;
            var ready_start = 0;
            var gone = 0;
            var signaled = 0;
            var stranded = 0;

            action Announce when (phase == 0 && pid > 0 && owners == 1 && scan_complete == 1) {
                phase = 1;
                owner_tab = tab;
                owner_pid = pid;
                owner_start = start;
                gone = 0;
            }
            // The READY answers the notice it was typed after: the fence's.
            action Ready when (phase == 1 && ready == 0) {
                ready = 1;
                ready_pid = owner_pid;
                ready_start = owner_start;
            }
            action OtherTab when (phase == 1 && tab == 1) {
                tab = 2;
                pid = 2;
                start = 2;
            }
            // Resumed by hand in the notice's own tab: after its process
            // exited, or with it alive and no longer holding the conversation.
            action Resume when (phase == 1 && tab == 1 && pid <= 1) {
                pid = 2;
                start = 2;
            }
            // The process the notice reached exits or crashes with no relaunch; the conversation
            // is nobody's until a process resumes it, unless one already holds it.
            action OwnerExits when (
                phase == 1 && gone == 0 && owner_pid == 1 && owner_start == 1
            ) {
                gone = 1;
                pid = if (pid == owner_pid && start == owner_start) { 0 } else { pid };
                start = if (pid == owner_pid && start == owner_start) { 0 } else { start };
            }
            // The notice's pid, recycled: another process holds the
            // conversation under it.
            action Recycled when (phase == 1 && gone == 1 && tab == 1 && pid == 0) {
                pid = 1;
                start = 3;
            }
            action DuplicateOwner when (phase == 1 && owners == 1) {
                owners = 2;
            }
            action IncompleteScan when (scan_complete == 1 && phase <= 1) {
                scan_complete = 0;
            }
            action Terminate when (
                phase == 1 && pid > 0 && ready == 1 &&
                (Buggy == 1 ||
                    (owners == 1 && scan_complete == 1 && owner_tab == tab &&
                     owner_pid == pid && owner_start == start))
            ) {
                phase = 2;
                signaled = 1;
            }
            // The notice's process is gone and one it never reached holds the conversation:
            // asked afresh, pending in a new round, the markers forgotten (`KeepRound`: kept,
            // and with them the gone process's READY). Its fence goes; nothing is owed to it.
            action Reask when (
                phase == 1 && pid > 0 && owners == 1 && scan_complete == 1 && gone == 1 &&
                Buggy == 0 && NoReask == 0 &&
                (if (owner_tab == tab && owner_pid == pid && owner_start == start) { 0 }
                 else { 1 }) == 1
            ) {
                phase = 0;
                ready = if KeepRound == 1 { ready } else { 0 };
                ready_pid = if KeepRound == 1 { ready_pid } else { 0 };
                ready_start = if KeepRound == 1 { ready_start } else { 0 };
                owner_tab = 0;
                owner_pid = 0;
                owner_start = 0;
            }
            // A visit that finds nothing to do (neither guard above holds): it waits on a notice
            // whose process is gone (`stranded`) or on one that can still be answered.
            action Look when (
                phase == 1 && pid > 0 && owners == 1 && scan_complete == 1 && stranded == 0 &&
                (if (
                    phase == 1 && pid > 0 && owners == 1 && scan_complete == 1 && gone == 1 &&
                    Buggy == 0 && NoReask == 0 &&
                    (if (owner_tab == tab && owner_pid == pid && owner_start == start) { 0 }
                     else { 1 }) == 1
                ) { 1 } else { 0 }) +
                (if (
                    phase == 1 && pid > 0 && ready == 1 &&
                    (Buggy == 1 ||
                        (owners == 1 && scan_complete == 1 && owner_tab == tab &&
                         owner_pid == pid && owner_start == start))
                ) { 1 } else { 0 }) == 0
            ) {
                stranded = gone;
            }

            invariant OnlyIssuerSignaled:
                signaled == 0 ||
                (owner_tab == tab && owner_pid == pid && owner_start == start);
            invariant NoDuplicateOwnerSignal: signaled == 0 || owners == 1;
            invariant NoPartialScanSignal: signaled == 0 || scan_complete == 1;
            invariant OnlyOnItsOwnAnswer:
                signaled == 0 || (ready_pid == pid && ready_start == start);
            invariant NeverWaitsOnAGoneNotice: stranded == 0;
        }
    }
}

/// The upgrade drain's bounds (`aterm_agent::harness::upgrade`, `DRAIN_S`, `HOLD_S`, `REASK_S`,
/// `MAX_ASKS`). After a notice each look takes one step: wait, void the READY answer, end the
/// agent, ask again, or give up. Between looks a person (a box nobody answers, a draft nobody
/// sends, standing `HOLD_S`), the agent's own work, its READY answer and a break of its
/// background work come and go. `waited` counts looks since the latest notice and `aged` looks
/// since its READY; both saturate at `Bound`, the drain and re-ask bound (`DRAIN_S == REASK_S`).
/// `asks` counts notices up to `MaxAsks`. A break (`brk`) is a turn end with only the agent's own
/// work running; there the upgrade takes only a notice, a give-up, or a void of an answer a person
/// held, and never ends the agent.
///
/// - `NoEndOnAHeldAnswer`: the agent is never ended on an answer a look saw a person hold at or
///   past the bound (`stale`), which would end it under a person answering the box late.
/// - `NoSilentWait`: the agent's own work is waited for and never ended, but once the re-ask
///   clock has run out only a person makes the upgrade wait without a word; otherwise it asks
///   again, naming what runs, or gives up (`silent`).
/// - `NeverEndsRunningWork`: aterm never ends the agent while its own work runs, at a break, or
///   on a status of Claude's own that is not `idle` (`cut`).
/// - `NoHastySupersede`: a READY answer gets a whole bound of its own before work it outlives
///   supersedes it by a re-ask or a give-up (`hasty`), never on the notice's clock alone.
///
/// Claude's own status lagging an idle screen (`lag`): `busy` or `shell` standing over a screen
/// the session's reader read idle, looks in a row, with nothing under the agent the kernel can
/// see. Work in the agent's own process (a background agent, a workflow) is no process under it,
/// and that status is the one word that says it runs, so the Drain step asks Claude's own `idle`
/// of the signal. A lag is held like the agent's work: never ended on (`cut`), its READY asked
/// again past the bound, then given up on. The notice and release it may take type one line and
/// end nothing (the never-strands model's). Knobs: `LagEnds` takes the lagging status for idle
/// (`NeverEndsRunningWork`); `LagHolds` keeps the restart to `idle` but waits on the READY behind
/// the lag for good (`NoSilentWait`).
///
/// `Buggy=1` never voids, so it waits on the person for good and ends the agent the moment they
/// let go; at a break, or under work a READY did not end, it waits silently for good; and it ends
/// the agent once the bound is past, work or no work, and supersedes a READY on the notice's
/// clock alone.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_drain_bound_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeDrainBound {
            const Buggy = 0;
            const LagEnds = 0;
            const LagHolds = 0;
            const Bound = 2;
            const MaxAsks = 2;
            var waited = 0;
            var aged = 0;
            var asks = 1;
            var ready = 1;
            var person = 0;
            var agent = 0;
            var brk = 0;
            var lag = 0;
            var stale = 0;
            var silent = 0;
            var cut = 0;
            var hasty = 0;
            var ended = 0;
            var gaveup = 0;

            action PersonHolds when (ended == 0 && gaveup == 0 && person == 0) { person = 1; }
            action PersonLets when (ended == 0 && gaveup == 0 && person == 1) { person = 0; }
            action AgentWorks when (ended == 0 && gaveup == 0 && agent == 0) { agent = 1; }
            action AgentRests when (ended == 0 && gaveup == 0 && agent == 1) { agent = 0; }
            action Answers when (ended == 0 && gaveup == 0 && ready == 0) {
                ready = 1;
                aged = 0;
            }
            action AtBreak when (ended == 0 && gaveup == 0 && brk == 0) { brk = 1; }
            action AtIdle when (ended == 0 && gaveup == 0 && brk == 1) { brk = 0; }
            // Claude's own status lags an idle screen, or catches up with it.
            action StatusLags when (ended == 0 && gaveup == 0 && lag == 0) { lag = 1; }
            action StatusIdle when (ended == 0 && gaveup == 0 && lag == 1) { lag = 0; }

            // At an idle point a READY waits on a person, the agent's own work, or a lagging
            // status (`LagEnds`: never); the re-ask bounds the work and the lag (`LagHolds`:
            // never the lag).
            action Wait when (
                ended == 0 && gaveup == 0 &&
                ((Buggy == 1 &&
                  (brk == 1 || (ready == 1 && (person == 1 || agent == 1 || lag == 1)))) ||
                 (brk == 1 && Buggy == 0 &&
                  ((ready == 0 && waited <= Bound - 1) ||
                   (ready == 0 && person == 1 && asks <= MaxAsks - 1) ||
                   (ready == 1 && aged <= Bound - 1 && (person == 0 || waited <= Bound - 1)))) ||
                 (brk == 0 && ready == 0 &&
                  (waited <= Bound - 1 || (person == 1 && asks <= MaxAsks - 1))) ||
                 (brk == 0 && ready == 1 && Buggy == 0 &&
                  (person == 1 || agent == 1 || (lag == 1 && LagEnds == 0)) &&
                  (person == 0 || waited <= Bound - 1) &&
                  ((agent == 0 && (lag == 0 || LagEnds == 1 || LagHolds == 1)) ||
                   person == 1 || aged <= Bound - 1)))
            ) {
                stale = if ready == 1 && person == 1 && waited > Bound - 1 { 1 } else { stale };
                silent = if person == 0 &&
                    ((ready == 0 && waited > Bound - 1) || (ready == 1 && aged > Bound - 1))
                    { 1 } else { silent };
                waited = if waited <= Bound - 1 { waited + 1 } else { waited };
                aged = if aged <= Bound - 1 { aged + 1 } else { aged };
            }
            action Void when (
                ended == 0 && gaveup == 0 && ready == 1 && person == 1 &&
                waited > Bound - 1 && Buggy == 0
            ) {
                ready = 0;
            }
            action ReAsk when (
                ended == 0 && gaveup == 0 && asks <= MaxAsks - 1 && person == 0 &&
                ((brk == 0 && ready == 0 && waited > Bound - 1) ||
                 (Buggy == 0 && brk == 1 &&
                  ((ready == 0 && waited > Bound - 1) || (ready == 1 && aged > Bound - 1))) ||
                 (Buggy == 0 && brk == 0 && ready == 1 &&
                  (agent == 1 || (lag == 1 && LagEnds == 0 && LagHolds == 0)) &&
                  aged > Bound - 1) ||
                 (Buggy == 1 && ready == 1 && agent == 1 && waited > Bound - 1))
            ) {
                hasty = if ready == 1 && aged <= Bound - 1 { 1 } else { hasty };
                asks = asks + 1;
                waited = 0;
                aged = 0;
                ready = 0;
            }
            action GiveUp when (
                ended == 0 && gaveup == 0 && asks > MaxAsks - 1 &&
                ((brk == 0 && ready == 0 && waited > Bound - 1) ||
                 (Buggy == 0 && brk == 1 &&
                  ((ready == 0 && waited > Bound - 1) ||
                   (ready == 1 && aged > Bound - 1 && person == 0))) ||
                 (Buggy == 0 && brk == 0 && ready == 1 &&
                  (agent == 1 || (lag == 1 && LagEnds == 0 && LagHolds == 0)) &&
                  aged > Bound - 1 && person == 0) ||
                 (Buggy == 1 && ready == 1 && agent == 1 && person == 0 && waited > Bound - 1))
            ) {
                hasty = if ready == 1 && aged <= Bound - 1 { 1 } else { hasty };
                gaveup = 1;
            }
            // Only on Claude's own `idle` (`LagEnds`: on a lagging status too).
            action Terminate when (
                ended == 0 && gaveup == 0 && ready == 1 && person == 0 &&
                ((brk == 0 && agent == 0 && (lag == 0 || LagEnds == 1)) ||
                 (Buggy == 1 && waited > Bound - 1))
            ) {
                cut = if brk == 1 || agent == 1 || lag == 1 { 1 } else { cut };
                ended = 1;
            }

            invariant NoEndOnAHeldAnswer: ended == 0 || stale == 0;
            invariant NoSilentWait: silent == 0;
            invariant NeverEndsRunningWork: cut == 0;
            invariant NoHastySupersede: hasty == 0;
        }
    }
}

/// The live upgrade never strands the agent it asked (`aterm_agent::harness::upgrade`,
/// `next_step`, `gate_announce`, `clock_held`, `gate_release`, `transcript_has_ready`,
/// `directed_since_ready`, and the driver's record transitions). A session parked at its usage
/// limit (`⚠ Usage limit reached · continuing automatically at 6am`) reads idle to the gates;
/// notices typed into it are all delivered at the reset, and an upgrade that has since given up
/// for good leaves the agent stopped on a READY nothing acts on.
///
/// `limited`: the session stands at its limit (the account comes and goes freely). `phase`: 0
/// pending, 1 announced, 2 gave up asking, 3 restarted and carried on, 4 stopped otherwise (the
/// owner's hold, a refused restart). `asks` counts notices to `MaxAsks` (`MAX_ASKS` scaled
/// down); `window`: the re-ask window since the last notice has run out. `unread`: a notice typed
/// while limited sits queued until the limit resets. `holding`: the agent read a notice and
/// stopped for the restart; `ready`: its last word is READY to a marker the record still keeps
/// (`live`). `owed`: one release line is owed. The conversation: `told`, someone (a person, a
/// peer, the supervisor) directed the agent since its latest READY or notice and it has not
/// answered yet; `directed`, it answered such a direction and took it up. Ghosts: `blind` (a
/// notice typed while limited), `stuck` (the upgrade's last word, a look at a readable point that
/// finds nothing left to do, found the agent holding), `overrode` (a restart acted on a READY
/// someone spoke after), `dropheld` (a release dropped while the agent held for the restart).
///
/// The agent's own work under a READY (composed with `HarnessUpgradeDrainBound`'s re-ask): past
/// the bound an announced upgrade asks again (`Supersede`) or, its asks spent, gives up over the
/// answer (`GiveUpOutlived`), and a gave-up one voids it (`Void`), owing the release. A person's
/// hold voids in either phase. Both are the environment's holds; Tier-1 checks each where the
/// model allows it.
///
/// A break that may never end (`brk`): the agent's turn is over and only work it started runs
/// (widowed `tail -f` shells under a Claude Code whose status reads `shell`, every screen a break).
/// The environment may leave a break, or never. There the upgrade ends nothing (`Restart` is an
/// idle point's), but the notice, the give-up, a void (standing for the restart a break never
/// takes) and the release, typed or dropped, go as at an idle point; the last word there repeats
/// for as long as the break lasts. `IdleOnlyRelease` allows the release, its drop and the gave-up
/// void only at an idle point, so a give-up at an endless break leaves the agent holding.
///
/// `Look` is derived from the reducer's own guards: enabled exactly where none of `Restart`,
/// `BuildHold`, `Release` or `DropRelease` is (the release's point, typed or dropped, is one
/// term), which the Tier-0 test checks state by state. Hand-written clauses ("a restart it would
/// take") would assume what is to be proved.
///
/// No stop is for good. `rest`: the stopped round has rested `RETRY_S`, the environment's clock
/// (`Rests`), begun again by every stop and by the void of a gave-up round's late READY, and not
/// held at a limit. `Rearm`: the reducer's new round (pending, asks, window and markers reset),
/// taken wherever the rest has run out, except over a gave-up round's late READY, which is still
/// acted on. An owed release is carried, typed or dropped until the rest runs out, at a break
/// too, and past it the new round's first notice supersedes it. `Look` is the upgrade's quiet
/// word (a stopped round's `wait:failed` while it rests is one). Ghost `stalled`: the quiet word
/// at a stopped round whose rest has run out, a permanent wait.
///
/// `DropRelease` is the driver's own rule (`release_void`, `directed`): its guard is the direction
/// the real code reads, and that it drops only an agent that took other work up, never one
/// holding for the restart, is the invariant `NoDropOverAHold`, not a guard conjunct (a guard
/// encoding the obligation would make the invariant vacuous).
///
/// Properties: `NoNoticeWhileLimited` (nothing typed into a limited session; the re-ask clock
/// neither runs there nor carries across it: `Elapse`, `LimitResets`); `NeverStranded` (an asked
/// agent is restarted and carried on, or released, never left holding under the last word);
/// `NoDropOverAHold` (a release is dropped only for an agent that took up direction given after
/// its last answer); `NoRestartOverDirection` (a restart never acts on a READY someone spoke
/// after); `NeverStalls` (a stopped round whose rest has run out always has a new round, or its
/// late READY's restart, enabled).
///
/// One knob per defect, each caught alone: `Terminal` (a stop is for good: no `Rearm`), `NoF1`
/// (notices and the clock at a limit), `NoF2` (a gave-up upgrade deaf to a late READY), `NoOwe`
/// (nothing abandoned owes a release), `NoType` (a release owed, never typed), `KeepReady` (a stop
/// keeps the markers and the release waits on a READY no phase acts on), `LastWhileOwed` (a
/// stop's own word is the host's last with a release owed), `StaleDirection` (a direction the
/// agent answered with READY still counts and drops the release its void owes),
/// `UnansweredDirection` (a direction counts before the agent answers it, so a look mid-turn drops
/// the release its later READY needed), `ReadyOverDirection` (a READY stays the last word past a
/// direction never answered: an Esc, a message met by a `<synthetic>` row), `IdleOnlyRelease`
/// (above). `Buggy = 1` is `NoF1`, `NoF2`, `NoOwe` and `Terminal` at once. Tier-1: aterm-agent's
/// `harness::upgrade_drive` tests drive the real reducer, gates, record transitions, transcript
/// readers and the host's reading of each step's word over every reachable state (a gave-up
/// round's late READY at a break among them: the real code must `Void` exactly where the model
/// voids), with the usage-limit trace as the caught negative control.
///
/// A build that fails to start (`upgrade::build_hold`; `fails`, the environment's: a relaunch
/// onto the round's target never registered, and no probe has proved it since; `BuildFails`,
/// `BuildStarts`). No agent is moved onto it: no notice (`Announce`, `Supersede`) and no
/// `Restart`. A round that asked its agent (announced, or given up with a late READY) is
/// abandoned at once (`BuildHold`: the stop, the markers forgotten, the release owed), and a
/// pending round that owes a release types it there (`Release` at phase 0), since no notice comes
/// to supersede it. The last word is said at a pending or announced round too while the build
/// fails (`wait:build-fails`). Two knobs, each caught by `NeverStranded`: `HeldAsked` (the build
/// hold only a wait where the round would move) and `HeldRelease` (a pending round's owed release
/// left to a notice that never comes).
///
/// One stated exception, outside the model: an agent found to be no job of a job-control shell is
/// refused and owed no release (`upgrade_drive::not_a_job`): the line is typed under the notice's
/// fences, the agent its shell's foreground job among them, and such an agent can never be proven
/// to read it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_never_strands_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeNeverStrands {
            const Buggy = 0;
            const NoF1 = 0;
            const NoF2 = 0;
            const NoOwe = 0;
            const NoType = 0;
            const KeepReady = 0;
            const LastWhileOwed = 0;
            const StaleDirection = 0;
            const UnansweredDirection = 0;
            const ReadyOverDirection = 0;
            const IdleOnlyRelease = 0;
            const Terminal = 0;
            const HeldAsked = 0;
            const HeldRelease = 0;
            const MaxAsks = 2;
            var limited = 0;
            var brk = 0;
            var phase = 0;
            var asks = 0;
            var window = 0;
            var unread = 0;
            var holding = 0;
            var ready = 0;
            var live = 0;
            var owed = 0;
            var told = 0;
            var directed = 0;
            var blind = 0;
            var stuck = 0;
            var overrode = 0;
            var dropheld = 0;
            var rest = 0;
            var stalled = 0;
            var fails = 0;

            // The account and the agent, as they please.
            action LimitHits when (limited == 0 && (phase <= 2 || phase == 4)) {
                limited = 1;
            }
            // The clock is held through the limit: an announced upgrade's window starts again
            // when the agent can read.
            action LimitResets when (limited == 1) {
                limited = 0;
                holding = if unread == 1 { 1 } else { holding };
                unread = 0;
                window = if (phase == 1 && Buggy == 0 && NoF1 == 0) { 0 } else { window };
            }
            // The agent answers READY, which answers a direction it had not answered yet
            // (`StaleDirection`: still counted).
            action AgentReady when (limited == 0 && holding == 1 && ready == 0 && live == 1) {
                ready = 1;
                directed = if ((StaleDirection == 1 || Buggy == 1) && told == 1) { 1 } else if ((StaleDirection == 1 || Buggy == 1)) { directed } else { 0 };
                told = 0;
            }
            // The agent goes on, with the direction it was given if one stands unanswered.
            action AgentGoesOn when (limited == 0 && holding == 1 && ready == 0) {
                holding = 0;
                directed = if told == 1 { 1 } else { directed };
                told = 0;
            }
            // Someone (a person, a peer, the supervisor) directs the agent holding for the
            // restart; a READY before it is no longer its last word (`ReadyOverDirection`: it
            // is).
            action Direct when (limited == 0 && holding == 1 && told == 0 && directed == 0) {
                told = 1;
                ready = if (ReadyOverDirection == 1 || Buggy == 1) { ready } else { 0 };
            }
            // The re-ask clock runs only while the session can read a notice.
            action Elapse when (
                phase == 1 && window == 0 && (limited == 0 || Buggy == 1 || NoF1 == 1)
            ) {
                window = 1;
            }
            // A break of the agent's own work: its turn over, work it started still running. It
            // may end, or never.
            action BreakBegins when (brk == 0) {
                brk = 1;
            }
            action BreakEnds when (brk == 1) {
                brk = 0;
            }
            // A stopped round's rest runs out (`RETRY_S` since it stopped), limited or not: the
            // round it lets start types nothing there.
            action Rests when ((phase == 2 || phase == 4) && rest == 0) {
                rest = 1;
            }
            // The round's target fails to start (a relaunch onto it, for any conversation,
            // never registered) and no probe has proved it since; or one does (or a newer build
            // is the target).
            action BuildFails when (fails == 0) {
                fails = 1;
            }
            action BuildStarts when (fails == 1) {
                fails = 0;
            }

            // The upgrade's steps.
            action Announce when (
                ready == 0 && fails == 0 && (limited == 0 || Buggy == 1 || NoF1 == 1) &&
                (phase == 0 || (phase == 1 && window == 1 && asks <= MaxAsks - 1))
            ) {
                phase = 1;
                asks = asks + 1;
                window = 0;
                live = 1;
                owed = 0;
                unread = limited;
                holding = if limited == 1 { holding } else { 1 };
                blind = if limited == 1 { 1 } else { blind };
                told = 0;
                directed = 0;
            }
            action GiveUp when (
                phase == 1 && window == 1 && asks == MaxAsks && ready == 0 &&
                (fails == 0 || HeldAsked == 1) &&
                (limited == 0 || Buggy == 1 || NoF1 == 1)
            ) {
                phase = 2;
                owed = if NoOwe == 1 { owed } else { 1 };
                rest = 0;
            }
            // Never at a break: the restart would end the work that runs.
            action Restart when (
                ready == 1 && live == 1 && limited == 0 && brk == 0 && fails == 0 &&
                (phase == 1 || (phase == 2 && Buggy == 0 && NoF2 == 0))
            ) {
                phase = 3;
                ready = 0;
                holding = 0;
                live = 0;
                owed = 0;
                overrode = if told == 1 { 1 } else { overrode };
                told = 0;
                directed = 0;
                rest = 0;
            }
            // The agent's own work outlived the READY past the bound (the environment's hold,
            // as `Void`'s is): an announced upgrade asks again, naming what runs, and the new
            // notice supersedes the answer.
            action Supersede when (
                ready == 1 && live == 1 && limited == 0 && phase == 1 && fails == 0 &&
                window == 1 && asks <= MaxAsks - 1
            ) {
                asks = asks + 1;
                window = 0;
                ready = 0;
                owed = 0;
                holding = 1;
                told = 0;
                directed = 0;
            }
            // Its asks spent, it gives up over the answer the work outlived; the answer stands
            // and the gave-up arm's `Void` releases it.
            action GiveUpOutlived when (
                ready == 1 && live == 1 && limited == 0 && phase == 1 &&
                (fails == 0 || HeldAsked == 1) &&
                window == 1 && asks == MaxAsks
            ) {
                phase = 2;
                owed = if NoOwe == 1 { owed } else { 1 };
                rest = 0;
            }
            // A person held the READY answer past the drain's bound (an announced upgrade's, or
            // the late answer a gave-up one hears), or the agent's own work held a gave-up
            // one's: at an idle point and at a break (`IdleOnlyRelease`: a gave-up one's only at
            // an idle point).
            action Void when (
                ready == 1 && live == 1 && limited == 0 && (fails == 0 || HeldAsked == 1) &&
                ((phase == 1 && window == 1) ||
                 (phase == 2 && Buggy == 0 && NoF2 == 0 && (brk == 0 || IdleOnlyRelease == 0)))
            ) {
                ready = 0;
                live = 0;
                owed = if NoOwe == 1 { owed } else { 1 };
                window = if (phase == 1 && Buggy == 0 && NoOwe == 0) { 0 } else { window };
                // A gave-up round's rest begins again at the void, so its release stands a
                // whole rest before a new round's notice.
                rest = 0;
            }
            // The build hold (`upgrade::build_hold`): a round that asked its agent, its target
            // failing to start, is abandoned at once, at a break or a limit too: the markers
            // forgotten, the release owed (`HeldAsked`: only a wait where it would move).
            action BuildHold when (
                fails == 1 && HeldAsked == 0 &&
                (phase == 1 || (phase == 2 && ready == 1 && live == 1))
            ) {
                phase = 4;
                ready = 0;
                live = 0;
                owed = if NoOwe == 1 { owed } else { 1 };
                rest = 0;
            }
            // The owner's hold, a refused plan or signal: the round is abandoned, its markers
            // with it unless `KeepReady`.
            action Abandon when (phase == 1 || (phase == 2 && ready == 1 && live == 1)) {
                phase = 4;
                ready = if KeepReady == 1 { ready } else { 0 };
                live = if KeepReady == 1 { live } else { 0 };
                owed = if NoOwe == 1 { owed } else { 1 };
                rest = 0;
            }
            // No stop is for good: a stopped round that has rested starts a new one (pending,
            // asks, window and markers reset) unless it gave up with a late READY in hand,
            // which is still acted on. An owed release is carried until the new round's first
            // notice supersedes it (`Terminal`: never).
            action Rearm when (
                (phase == 2 || phase == 4) && rest == 1 && Buggy == 0 && Terminal == 0 &&
                (phase == 4 || ready == 0 || live == 0)
            ) {
                phase = 0;
                asks = 0;
                window = 0;
                ready = 0;
                live = 0;
                rest = 0;
            }
            // The release's point: it waits behind a READY only where the phase acts on it
            // (`KeepReady`: behind any), and is typed unless the agent took up direction given
            // after its last answer (`UnansweredDirection`: or was merely given one), at a break
            // as at an idle point (`IdleOnlyRelease`: never at a break). An announced round's
            // window belongs to the build hold while the build fails; a pending round's, its
            // notice held by the build, to the release (`HeldRelease`: never).
            action Release when (
                owed == 1 && limited == 0 && NoType == 0 &&
                (brk == 0 || IdleOnlyRelease == 0) &&
                (ready == 0 || (phase == 4 && KeepReady == 0)) &&
                (phase == 2 || phase == 4 ||
                 (phase == 1 && window == 0 && (fails == 0 || HeldAsked == 1)) ||
                 (phase == 0 && fails == 1 && HeldRelease == 0)) &&
                directed == 0 && ((UnansweredDirection == 0 && Buggy == 0) || told == 0) &&
                (rest == 0 || Buggy == 1 || Terminal == 1 || (phase == 2 && ready == 1 && live == 1))
            ) {
                owed = 0;
                holding = 0;
                live = 0;
                ready = 0;
            }
            // Dropped there instead (`release_void`, `directed`): nothing owed, the markers
            // forgotten. Nothing is typed, so a limit holds the drop back only where it holds
            // the step's word (an announced upgrade waits `limited`; a stopped one's word is its
            // stop).
            action DropRelease when (
                owed == 1 && NoType == 0 &&
                (brk == 0 || IdleOnlyRelease == 0) &&
                (ready == 0 || (phase == 4 && KeepReady == 0)) &&
                (phase == 2 || phase == 4 ||
                 (phase == 1 && window == 0 && limited == 0 && (fails == 0 || HeldAsked == 1)) ||
                 (phase == 0 && fails == 1 && limited == 0 && HeldRelease == 0)) &&
                (directed == 1 || ((UnansweredDirection == 1 || Buggy == 1) && told == 1)) &&
                (rest == 0 || Buggy == 1 || Terminal == 1 || (phase == 2 && ready == 1 && live == 1))
            ) {
                owed = 0;
                live = 0;
                ready = 0;
                dropheld = if holding == 1 { 1 } else { dropheld };
            }
            // The quiet word at a point the agent can read: nothing left the upgrade would do
            // now. No restart (at a break the drain's void of a gave-up upgrade's READY stands
            // for it; while the build fails, none at all), no release typed or dropped, no new
            // round (`LastWhileOwed`: a stop's own word said over a release still owed), no
            // build hold. At a break it is the word the break repeats; past a stopped round's
            // rest, a permanent wait (`stalled`). While the build fails a pending or announced
            // round says it too (`wait:build-fails`).
            action Look when (
                (phase == 2 || phase == 4 || (fails == 1 && phase <= 1)) &&
                limited == 0 && stuck == 0 &&
                (if (
                    ready == 1 && live == 1 && limited == 0 &&
                    phase == 2 && Buggy == 0 && NoF2 == 0 &&
                    (brk == 0 || IdleOnlyRelease == 0) && (fails == 0 || brk == 1)
                ) { 1 } else { 0 }) +
                (if (LastWhileOwed == 1 && phase == 4) { 0 } else if (
                    owed == 1 && limited == 0 && NoType == 0 &&
                    (brk == 0 || IdleOnlyRelease == 0) &&
                    (ready == 0 || (phase == 4 && KeepReady == 0)) &&
                    (phase == 2 || phase == 4 ||
                     (phase == 1 && window == 0 && (fails == 0 || HeldAsked == 1)) ||
                     (phase == 0 && fails == 1 && HeldRelease == 0))
                ) { 1 } else { 0 }) +
                (if (
                    (phase == 2 || phase == 4) && rest == 1 && Buggy == 0 && Terminal == 0 &&
                    (phase == 4 || ready == 0 || live == 0)
                ) { 1 } else { 0 }) +
                (if (
                    fails == 1 && HeldAsked == 0 &&
                    (phase == 1 || (phase == 2 && ready == 1 && live == 1))
                ) { 1 } else { 0 }) == 0
            ) {
                stuck = holding;
                stalled = if rest == 1 { 1 } else { stalled };
            }

            invariant NoNoticeWhileLimited: blind == 0;
            invariant NeverStranded: stuck == 0;
            invariant NoDropOverAHold: dropheld == 0;
            invariant NoRestartOverDirection: overrode == 0;
            invariant NeverStalls: stalled == 0;
        }
    }
}

/// The end of the agent's own work is the upgrade's point (aterm-agent `harness/upgrade.rs`,
/// `next_step` over `Facts::work_ended`; `harness/upgrade_drive.rs`, `St::work_seen`,
/// `St::time_work` and the visit's same-visit re-arm). An agent that answers every notice with
/// "not yet, my workflow is still running" can only be moved at the first idle point after that
/// work, with nothing under it. A wait there that owns no turn end (`awaiting-ready` within the
/// window, `failed` while resting) lets the loop's `keep going` take the point and start new
/// work, so the round cycles for ever.
///
/// No wait there outlives the re-ask's window. The ask at the end of the work goes once the
/// restart's gate is open but for the READY, and that gate never reads past Claude's status: a
/// `shell` or `busy` left over the idle screen after a background shell ended waits
/// `status-stale` at every look. Inside the window the gate's wait holds the point; past it the
/// re-ask decides: a line through the lag (the notice's own gate reads past it) or, its asks
/// spent, the give-up.
///
/// `phase` 1 announced with no READY, 2 resting after a give-up (a late READY is out of the
/// model); `asks` the round's notices (`Cap` stands for `MAX_ASKS`, the Tier-1 shifting the count
/// onto the real one); `worked` the agent's own work seen since the latest notice; `screen` 0 an
/// idle point with nothing under the agent, 1 a break of its own work; `lag` Claude's status reads
/// its own work over that idle screen and never catches up (the restart's gate waits, a line's does
/// not); `late` the re-ask's window since the latest notice has passed; `looked` the host has taken
/// its step at this point (it goes first: `IdleHost::at_idle`), `owns` that step owns the point.
/// `HostAsks` is the notice at the end of the work (from a rest, re-armed: its asks start again);
/// `HostHolds` the restart gate's wait there; `HostReasks` and `HostGivesUp` the re-ask past the
/// window; `HostWaits` the quiet word owning nothing. Ghosts: `handed` (the loop continued the
/// agent at an idle point after its work was seen and ended, the gate open, the round open),
/// `nagged` (an ask at the end of work no look saw), `stuck` (an announced round's look past its
/// window waited the restart gate's word). Dials, each caught alone: `Buggy = 1` (the quiet word
/// there, an ask anywhere, the wait past the window); `Nag = 1` (an ask with no work seen since
/// the notice); `Hold = 1` (the wait at the end of the work holds past the window).
///
/// Tier-1: aterm-agent's `harness::upgrade` tests (`the_work_end_reducer_is_bound_to_its_model`)
/// drive the real `next_step` over every reachable idle point: it asks (`Announce`, or `Rearm`
/// from a rest) exactly where `HostAsks` or `HostReasks` is enabled, gives up exactly where
/// `HostGivesUp` is, waits the restart gate's word exactly where `HostHolds` is, and says its
/// quiet word exactly where `HostWaits` is.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_work_end_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeWorkEnd {
            const Buggy = 0;
            const Nag = 0;        // 1: the ask needs no work seen since the notice
            const Hold = 0;       // 1: the wait at the end of the work outlives the window
            const Cap = 3;        // MAX_ASKS: the re-ask past it gives up
            var phase = 1;
            var asks = 1;
            var worked = 0;
            var screen = 0;
            var lag = 0;
            var late = 0;
            var looked = 0;
            var owns = 0;
            var handed = 0;
            var nagged = 0;
            var stuck = 0;

            // The agent's own work runs: a break of it, seen by the round's look there.
            action WorkRuns when (screen == 0) {
                screen = 1;
                worked = 1;
                looked = 0;
                owns = 0;
            }
            // Its work ends: an idle point with nothing under the agent.
            action WorkEnds when (screen == 1) {
                screen = 0;
                looked = 0;
                owns = 0;
            }
            // Claude's status keeps reading its own work over the idle screen and never catches
            // up.
            action StatusLags when (screen == 0 && lag == 0) {
                lag = 1;
                looked = 0;
                owns = 0;
            }
            // The re-ask's window since the latest notice runs out.
            action WindowPasses when (phase == 1 && late == 0) {
                late = 1;
                looked = 0;
                owns = 0;
            }
            // Its asks spent at breaks of the work, the round gives up and rests.
            action GivesUp when (phase == 1 && screen == 1) {
                phase = 2;
            }
            // The host's step at the end of the work, the restart's gate open: the notice (a
            // rest re-armed first), owning the point.
            action HostAsks when (
                screen == 0 && looked == 0 && lag == 0 &&
                (worked == 1 || Nag == 1 || Buggy == 1)
            ) {
                nagged = if worked == 0 { 1 } else { nagged };
                asks = if phase == 2 { 1 } else if asks <= Cap - 1 { asks + 1 } else { asks };
                phase = 1;
                worked = 0;
                late = 0;
                looked = 1;
                owns = 1;
            }
            // The restart's gate waits there (the status lags): a settle's word owning the
            // point, inside the window or through a rest.
            action HostHolds when (
                screen == 0 && looked == 0 && worked == 1 && lag == 1 &&
                (phase == 2 || late == 0 || Hold == 1 || Buggy == 1)
            ) {
                stuck = if (phase == 1 && late == 1) { 1 } else { stuck };
                looked = 1;
                owns = 1;
            }
            // Past the window the re-ask: its line goes through the lag.
            action HostReasks when (
                phase == 1 && screen == 0 && looked == 0 && late == 1 && asks <= Cap - 1 &&
                (worked == 0 || (lag == 1 && Hold == 0))
            ) {
                asks = asks + 1;
                worked = 0;
                late = 0;
                looked = 1;
                owns = 1;
            }
            // Or, its asks spent, the give-up: the round rests.
            action HostGivesUp when (
                phase == 1 && screen == 0 && looked == 0 && late == 1 && asks > Cap - 1 &&
                (worked == 0 || (lag == 1 && Hold == 0))
            ) {
                phase = 2;
                late = 0;
                looked = 1;
                owns = 0;
            }
            // Or its quiet word — `awaiting-ready`, `failed` — owning nothing.
            action HostWaits when (
                screen == 0 && looked == 0 && (worked == 0 || Buggy == 1) &&
                (phase == 2 || late == 0)
            ) {
                looked = 1;
                owns = 0;
            }
            // The loop continues the agent at a point its host owns nothing of; the next point
            // comes after that turn.
            action LoopContinues when (screen == 0 && looked == 1 && owns == 0) {
                handed = if (worked == 1 && lag == 0) { 1 } else { handed };
                looked = 0;
            }

            invariant TheEndOfItsWorkIsTheUpgrades: handed == 0;
            invariant AnAskFollowsWorkItSaw: nagged == 0;
            invariant NoWaitOutlivesTheReaskWindow: stuck == 0;
        }
    }
}

/// The login wall (`aterm_agent::harness::upgrade`: `gate_announce`, `next_step`,
/// `announce_asks`, `transcript_login`, `clock_held` and the driver's `login_facts`;
/// `aterm_agent::supervise::policy::turn_end`: `decide_turn_end`'s auth arm and its login hold;
/// both over `aterm_phase`'s wall reader). When the login expires, every turn ends in
/// milliseconds on Claude Code's synthetic `authentication_failed` row (`⏺ Login expired · Please
/// run /login`); read as idle, it draws continuations and notices that never reach the model, a
/// give-up on them, and a READY after the login that answers a stopped upgrade.
///
/// The session: `login` (signed in), `wall` (its last turn ended on the wall, whose row the
/// screen shows), `stood` (the transcript's last word on the login is the wall: set by every turn
/// the wall answers, cleared by `Login successful`, a model answer or a relaunch), `back` (the
/// screen says `Login successful`). The supervisor: `track` (`/login` typed), `told` (the owner
/// told). The upgrade: `phase` 0 pending, 1 announced, 2 gave up, 3 restarted; `asks` counted to
/// `MaxAsks` (`MAX_ASKS` scaled down); `window` (the re-ask window has run out); `unread` (the
/// latest notice's own turn was the wall's); `got` (asks the model received); `dark` (the wall
/// stood in the running window); `holding` (the agent read a notice and winds down); `ready` (its
/// READY is its last word); `inherited` (an older build's state, `Inherit`). Ghosts: `blind` (the
/// upgrade typed where the wall showed or stood), `spent` (a give-up with fewer than `MaxAsks`
/// received, or over a window the wall darkened), `futile` (a continuation into a login seen gone
/// and not back), `untold` (the supervisor acted at a wall before the owner was told).
///
/// `Inherit` is an older build's give-up on `MaxAsks` notices the wall answered
/// (`failed:unanswered`), which the current build takes back (`Announce` from phase 2). Not
/// modelled: the upgrade's ownership of turn ends after an announcement (the supervisor types
/// nothing there; here it may), and the release line an abandoned notice owes
/// (`harness_upgrade_never_strands_model`).
///
/// The renewable login (`aterm_agent::harness::accounts`' login lifecycle; the policy's
/// `login_label` arms): `rostered` (the session's login is a roster row with an email, `Enrol`
/// from a clean session), `renewed` (a renewal whose check answered the row's account bound a new
/// login the session is not on yet). The owner renews (`Renew`) at any time. Once renewed the
/// session moves onto the new login, relaunched at its wall (`Relaunch`) or at an idle point
/// ahead of any wall (`Follow`).
///
/// THE LOST LOGIN CARRIED (the owner's ruling of 2026-10-19, after v0.106.0 left one to them,
/// silent: a one-row roster's login was waited at for a renewal nobody was asked for, and the
/// wall over a finished task was never decided). Every lost login with no renewed one to follow
/// — rostered or not — gets `/login` ONCE (`TypeLogin`), and the owner is told now (`TellNow`:
/// the exception the host asks on the glass) wherever the rest is theirs: the sign-in that
/// `/login` opened (abstracted here as `/login` typed: `told`), a `/login` left. `done` is a
/// wall that answered the owner's own prompt to a finished task in milliseconds (`WallOverDone`:
/// no read saw that turn run, and the prompt's key is the person's). The dialog's method picker
/// (`picker`: 1 behind aterm's `/login`) is answered with the method the session's login already
/// is (`PickMethod`, `known`: a roster row or the profile's plan, `Profiled`), never a guess;
/// left to the owner otherwise. `Dismiss` is Esc out of aterm's dialog (`Login interrupted`).
/// A PERSON'S OWN `/login`: on a working session its picker (`picker` 2, `PersonPicker`) is
/// theirs whole (`PersonPicked`), and at a lost login one they left (`PersonLeft`: Esc, the
/// incident of 2026-10-19) is their answer — no `/login` of aterm's follows it, the person's
/// next prompt answered by the wall again (`PersonPrompt`) included, and the owner is told the
/// sign-in is theirs. ONE SIGN-IN PER LOGIN (the review's F7: every tab on a shared login typed
/// its own `/login`, and the owner met as many browser sign-ins): another tab of the host on the
/// same login (`elsewhere`) may take the login's one sign-in first (`OtherCarries`: its `/login`,
/// the owner told there), and this tab waits on it, typing nothing; the login comes back there
/// (`OtherBack`: `elsewhere` 2), and this tab's turn is delivered again (`Continue` at its wall:
/// the model's answer is its lift), or that tab closes with its sign-in unfinished
/// (`OtherGone`), and this tab's `/login` is the login's. The login may come back in another
/// tab while this tab's own `/login` carries its sign-in, too (`BackElsewhere`: the owner signed
/// in there, this tab's dialog standing or left; the review of the login carry, 2026-10-20, whose
/// carrier never read it back): its turn is delivered again all the same — or, a renewal
/// standing, it moves onto the renewed login (`Relaunch`) — and nobody is told to sign in a login
/// that is back: while aterm's dialog still stands, the owner is told to close it (`TellClose`;
/// it takes no keys of the loop's, and its picker is answered by nobody), and the turn goes once
/// it is left (`Dismiss`). A renewal made while aterm's `/login` was up is followed once that
/// `/login` is left (`Relaunch` at `Login interrupted`), never told as a sign-in to make again:
/// while its dialog stands the owner is told to close it (`TellClose`) and its picker is answered
/// by nobody (the review of the login carry, acct3-3: the dialog asked for a browser sign-in that
/// would bring back the replaced login). More ghosts: `stranded` (a session left on a dead login, continued or signed in,
/// while a renewed one stood ready), `checked` (a check run under an idle login, which spends its
/// single-use refresh token: the login dies), `silent` (the supervisor rested
/// at a lost login with nothing typed and nobody told), `relooped` (`/login` typed again into a
/// loss its `/login` already met), `guessed` (a method picked that nothing said the session
/// uses), `overrode` (`/login` typed into a loss a person's own `/login` left), `overpicked` (a
/// person's own picker answered), `doubled` (`/login` typed while another tab carries the
/// login's sign-in, or after it came back there, or once a renewal replaced it), `misled` (the
/// owner told now to sign in a login that came back in another tab), `reasked` (the owner told now
/// to sign in at aterm's dialog over a renewal).
///
/// Properties: `NoNoticeAtTheWall`, `NoGiveUpUnread`, `NoFutileContinue`, `TheOwnerIsToldFirst`,
/// `NoSessionLeftOnADeadLogin`, `NoCheckUnderAnIdleLogin`, `NoLostLoginLeftSilent`,
/// `LoginTypedOncePerLoss`, `NoMethodGuessed`, `NoLoginOverAPersonsCancel`,
/// `NoPersonsPickerAnswered`, `OneSignInPerLogin`, `NoSignInAskedOfALoginBack`,
/// `NoSignInAskedOverARenewal`. One knob per defect, each caught alone: `SignInOverBack` (the
/// review of the login carry, 2026-10-20: the sign-in notice of aterm's dialog stands over a login
/// back in another tab), `SignInOverRenewed` (acct3-3: it stands over a renewal made while it was
/// up), `PickAfterBack` (aterm's picker answered after the login came back elsewhere or was
/// renewed: a browser sign-in nobody needs), `NoFollowAfterLogin` (a renewal made while aterm's `/login` was up is not followed
/// once it is left: the owner is told to sign the replaced login in again), `NoGate` (the upgrade's gate misses the wall), `NoRefund` (a notice
/// the wall answered is counted and given up on), `ClockAtWall` (the window runs at the wall and
/// the lift does not restart it), `NoSee` (the supervisor's reader misses the wall), `NoHold` (a
/// lost login is continued once its row leaves the screen), `WaitOnRoster` (v0.106.0: a rostered
/// login is waited at, nothing typed and nobody told), `NoFollow` (a renewed login is not followed:
/// the session continues or signs in the dead one), `CheckIdle` (the renewal checks the idle login
/// it replaces), `NoDoneWall` (v0.106.0: a wall over a finished task is no point, nothing typed and
/// nobody told), `GraceHolds` (the prompt's own key holds `/login` as a person's), `Relogin`
/// (`/login` typed again after `Login interrupted`), `GuessMethod` (the picker answered with no
/// method known), `ReloginOverLeft` (`/login` typed after a person's own was left), `PickAnyPicker`
/// (any `/login`'s picker answered, a person's own included), `NoLatch` (each tab types its own
/// `/login`), `NoCarrierBack` (a tab whose loss had a `/login` never reads the login back from
/// another tab: its turn stays stopped under the notice of a sign-in). The dials, each a family
/// whose space stays inside the interpreter's budget (ten defects at once, or four, outran it):
/// `Buggy = 1` the upgrade's three (`NoGate`, `NoRefund`, `ClockAtWall`), `2` the supervisor's
/// reader (`NoSee`, `NoHold`), `3` v0.106.0's walls and the renewal (`WaitOnRoster`, `NoFollow`,
/// `CheckIdle`, `NoDoneWall`, `GraceHolds`), `4` `/login` typed again (`Relogin`,
/// `ReloginOverLeft`), `5` a picker answered (`GuessMethod`, `PickAnyPicker`), `6` one sign-in per
/// login (`NoLatch`, `NoCarrierBack`), `7` a sign-in asked over a renewal (`SignInOverRenewed`).
/// Tier-1: aterm-agent's `conformance_login_wall`, over the
/// real readers, reducer, turn-end decider, exception, picker gate and policy and renewal on every
/// reachable state.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_login_wall_model() -> Model {
    aterm_spec::ty_model! {
        HarnessLoginWall {
            const Buggy = 0;
            const NoGate = 0;
            const NoRefund = 0;
            const ClockAtWall = 0;
            const NoSee = 0;
            const NoHold = 0;
            const WaitOnRoster = 0;
            const NoFollow = 0;
            const CheckIdle = 0;
            const NoDoneWall = 0;
            const GraceHolds = 0;
            const Relogin = 0;
            const GuessMethod = 0;
            const ReloginOverLeft = 0;
            const PickAnyPicker = 0;
            const NoLatch = 0;
            const NoCarrierBack = 0;
            const SignInOverBack = 0;
            const PickAfterBack = 0;
            const NoFollowAfterLogin = 0;
            const SignInOverRenewed = 0;
            const MaxAsks = 2;
            var login = 1;
            var wall = 0;
            var stood = 0;
            var back = 0;
            var track = 0;
            var told = 0;
            var phase = 0;
            var asks = 0;
            var window = 0;
            var unread = 0;
            var got = 0;
            var dark = 0;
            var holding = 0;
            var ready = 0;
            var inherited = 0;
            var blind = 0;
            var spent = 0;
            var futile = 0;
            var untold = 0;
            var rostered = 0;
            var renewed = 0;
            var stranded = 0;
            var checked = 0;
            var done = 0;
            var picker = 0;
            var known = 0;
            var silent = 0;
            var relooped = 0;
            var guessed = 0;
            var left = 0;
            var overrode = 0;
            var overpicked = 0;
            var elsewhere = 0;
            var doubled = 0;
            var misled = 0;
            var reasked = 0;

            // The account and the session, as they please: the login goes; a turn someone else
            // began (a background completion, a /loop wakeup) is answered by the wall, or the
            // owner's own prompt to a finished task, in milliseconds; the person's `/login`
            // finishes (`Login successful`) and the window restarts from the lift
            // (`ClockAtWall`: it does not); Esc leaves a `/login` dialog with no login.
            action Expire when (login == 1 && picker <= 1 && elsewhere == 0) {
                login = 0;
            }
            action WallHit when (login == 0 && wall == 0) {
                wall = 1;
                stood = 1;
                back = 0;
                dark = if (phase == 1 && window == 0) { 1 } else { dark };
            }
            action WallOverDone when (
                login == 0 && wall == 0 && stood == 0 && track == 0 && back == 0 && done == 0
            ) {
                wall = 1;
                stood = 1;
                done = 1;
                dark = if (phase == 1 && window == 0) { 1 } else { dark };
            }
            action LoginBack when (login == 0 && stood == 1) {
                login = 1;
                elsewhere = 0;
                wall = 0;
                stood = 0;
                back = 1;
                done = 0;
                picker = 0;
                left = 0;
                window = if (phase == 1 && (Buggy == 0 || Buggy > 1) && ClockAtWall == 0) {
                    0
                } else {
                    window
                };
                dark = if ((Buggy == 0 || Buggy > 1) && ClockAtWall == 0) { 0 } else { dark };
            }
            action Dismiss when (track == 1 && wall == 1 && (login == 0 || elsewhere == 2)) {
                wall = 0;
                picker = 0;
            }
            // A person's own `/login`: on a working session, its picker theirs to answer; at a
            // lost login aterm has not typed `/login` into yet, left with Esc (`Login
            // interrupted`), and their next prompt answered by the wall again.
            action PersonPicker when (picker == 0 && track == 0 && login == 1 && wall == 0) {
                picker = 2;
            }
            action PersonPicked when (picker == 2) {
                picker = 0;
            }
            action PersonLeft when (wall == 1 && track == 0 && left == 0 && login == 0) {
                wall = 0;
                left = 1;
            }
            action PersonPrompt when (left == 1 && wall == 0 && back == 0 && login == 0) {
                wall = 1;
            }
            // ANOTHER TAB OF THIS HOST ON THE SAME LOGIN: it takes the login's one
            // sign-in first (its `/login`, the owner told there), the login comes
            // back there, or the tab closes with its sign-in unfinished.
            action OtherCarries when (
                stood == 1 && login == 0 && elsewhere == 0 && track == 0 && renewed == 0 && left == 0
            ) {
                elsewhere = 1;
                told = 1;
            }
            action OtherBack when (elsewhere == 1) {
                login = 1;
                elsewhere = 2;
            }
            action OtherGone when (elsewhere == 1) {
                elsewhere = 0;
                told = if (track == 0) { 0 } else { told };
            }
            // …or the owner signs the login in in another tab while this tab's
            // own `/login` carries its sign-in (its dialog standing, or left).
            action BackElsewhere when (
                track == 1 && login == 0 && elsewhere == 0 && back == 0 && renewed == 0
            ) {
                login = 1;
                elsewhere = 2;
            }
            // A state an older build left: it gave up on every notice, each answered by the wall.
            action Inherit when (phase == 0 && asks == 0 && inherited == 0 && stood == 1) {
                phase = 2;
                asks = MaxAsks;
                unread = 1;
                window = 1;
                inherited = 1;
            }

            // The login made renewable: a roster row with an email names it (`accounts
            // adopt-default`), enrolled from a clean session, which says its method too. A
            // renewal comes at any time, through a staging directory whose check runs under the
            // new login alone (`CheckIdle`: under the idle one it replaces, spending that one's
            // refresh token). The profile's plan says the method of a login no row names.
            action Enrol when (
                rostered == 0 && login == 1 && wall == 0 && stood == 0 && track == 0 && back == 0
            ) {
                rostered = 1;
                known = 1;
            }
            action Profiled when (known == 0) {
                known = 1;
            }
            action Renew when (rostered == 1 && renewed == 0) {
                renewed = 1;
                checked = if ((Buggy == 3 || CheckIdle == 1) && login == 1) { 1 } else { checked };
                login = if (Buggy == 3 || CheckIdle == 1) { 0 } else { login };
            }

            // The supervisor, at a point it reads the wall at: `/login` once, rostered or not,
            // over a finished task too, its key no person's — never while a renewed login stands
            // ready (`NoFollow`: it does, leaving the session on the dead one), nor again into a
            // loss its `/login` met (`Relogin`: after `Login interrupted`). (`WaitOnRoster`,
            // `NoDoneWall`, `GraceHolds`: not typed there.) The dialog's picker shows.
            action TypeLogin when (
                ((wall == 1 && track == 0 &&
                    (renewed == 0 || Buggy == 3 || NoFollow == 1) &&
                    (Buggy <= 1 || Buggy > 2) && NoSee == 0 &&
                    (rostered == 0 || ((Buggy <= 2 || Buggy > 3) && WaitOnRoster == 0)) &&
                    (done == 0 ||
                        ((Buggy <= 2 || Buggy > 3) && NoDoneWall == 0 && GraceHolds == 0)) &&
                    (left == 0 || Buggy == 4 || ReloginOverLeft == 1) &&
                    (elsewhere == 0 || Buggy == 6 || NoLatch == 1)) ||
                    (track == 1 && wall == 0 && stood == 1 && back == 0 && renewed == 0 &&
                        (Buggy == 4 || Relogin == 1)))
            ) {
                relooped = if (track == 1) { 1 } else { relooped };
                overrode = if (left == 1) { 1 } else { overrode };
                doubled = if (elsewhere == 0) { doubled } else { 1 };
                stranded = if (renewed == 1) { 1 } else { stranded };
                track = 1;
                told = 1;
                picker = 1;
            }
            // The owner told now (the exception the host asks on the glass, not escalated)
            // wherever the rest of the loss is theirs: a `/login` left (aterm's, or a person's
            // own with no renewed login to move onto) until the login is back, here or in
            // another tab (`NoCarrierBack`: the notice of a sign-in stands over a login back
            // there). The sign-in `/login` opened in the browser is told with it (`TypeLogin`
            // sets `told`); a `/login` about to be typed, a move and its wait tell nothing:
            // aterm is still at it.
            //
            // Not at aterm's `/login` left once a renewal stood ready: the tab moves onto the
            // renewed login (`Relaunch`; `NoFollowAfterLogin`: the owner is told to sign the
            // login it replaced in again, a second sign-in for one loss). Nor while that
            // `/login`'s dialog stands over a renewal: nothing is left to sign in there
            // (`TellClose`; `SignInOverRenewed`: the dialog's sign-in notice stands, a browser
            // sign-in that would bring back the login the renewal replaced).
            action TellNow when (
                back == 0 && (track == 1 || (left == 1 && renewed == 0)) &&
                (elsewhere <= 1 || Buggy == 6 || NoCarrierBack == 1 ||
                    (wall == 1 && track == 1 && SignInOverBack == 1)) &&
                (track == 0 || renewed == 0 ||
                    (wall == 1 && (SignInOverRenewed == 1 || Buggy == 7)) ||
                    (wall == 0 && NoFollowAfterLogin == 1))
            ) {
                misled = if (elsewhere == 2) { 1 } else { misled };
                reasked = if (track == 1 && wall == 1 && renewed == 1) { 1 } else { reasked };
                stranded = if (track == 1 && wall == 0 && renewed == 1) { 1 } else { stranded };
                told = 1;
            }
            // The login back in another tab, or a renewal made, while aterm's own `/login`
            // dialog still stands here: nothing is left to sign in, and the owner is told to
            // close the dialog (it takes no keys of the loop's), never to finish a sign-in
            // (`SignInOverBack`, `SignInOverRenewed`: the dialog's sign-in notice stands over
            // the login back, or over the renewal).
            action TellClose when (
                back == 0 && track == 1 && wall == 1 &&
                ((elsewhere == 2 && SignInOverBack == 0) ||
                    (renewed == 1 && SignInOverRenewed == 0))
            ) {
                told = 1;
            }
            // Where v0.106.0 rested: a lost login with nothing typed and nobody told.
            action Rest when (
                wall == 1 && track == 0 && told == 0 && renewed == 0 &&
                ((rostered == 1 && (Buggy == 3 || WaitOnRoster == 1)) ||
                    (done == 1 && (Buggy == 3 || NoDoneWall == 1)))
            ) {
                silent = 1;
            }
            // The picker answered with the method the session's login already is; with none
            // known, it is left to the owner (`GuessMethod`: a method is picked anyway).
            // Not once the login came back in another tab, nor once a renewal replaced it:
            // nothing is left to sign in, and a method picked opens a browser sign-in nobody
            // needs (`PickAfterBack`).
            action PickMethod when (
                (picker == 1 && ((elsewhere <= 1 && renewed == 0) || PickAfterBack == 1) &&
                    (known == 1 || Buggy == 5 || GuessMethod == 1)) ||
                (picker == 2 && (Buggy == 5 || PickAnyPicker == 1))
            ) {
                doubled = if (picker == 1 && (elsewhere == 2 || renewed == 1)) {
                    1
                } else {
                    doubled
                };
                guessed = if (picker == 1 && known == 0) { 1 } else { guessed };
                overpicked = if (picker == 2) { 1 } else { overpicked };
                picker = 0;
            }
            // Renewed: a session at its wall is relaunched under the renewed login (its
            // continuation follows) — not through a `/login` dialog of aterm's still standing
            // there, whose sign-in is the owner's to finish or leave (it takes no keys of the
            // loop's); once that `/login` is left, its sign-in is over and the tab moves
            // (`NoFollowAfterLogin`: not then, short of the login back in another tab, which
            // leaves it nothing to sign in); the old login back there meanwhile holds none of
            // it (the relaunch ends this tab's loss, its track included) — …
            action Relaunch when (
                renewed == 1 && (wall == 1 || stood == 1) && (login == 0 || elsewhere == 2) &&
                (track == 0 || (wall == 0 && (elsewhere == 2 || NoFollowAfterLogin == 0))) &&
                (Buggy <= 2 || Buggy > 3) && NoFollow == 0
            ) {
                login = 1;
                wall = 0;
                stood = 0;
                back = 0;
                renewed = 0;
                done = 0;
                picker = 0;
                left = 0;
                elsewhere = 0;
                track = 0;
                window = if (phase == 1 && ClockAtWall == 0) { 0 } else { window };
                dark = if (ClockAtWall == 0) { 0 } else { dark };
            }
            // …and one at an idle point, ahead of any wall, follows it.
            action Follow when (
                renewed == 1 && wall == 0 && stood == 0 && (Buggy <= 2 || Buggy > 3) && NoFollow == 0
            ) {
                login = 1;
                renewed = 0;
                elsewhere = 0;
            }
            // At a point it reads no wall at, a continuation, never while a lost login's track
            // stands and the screen does not say it is back (`NoHold`: it does), nor while a
            // renewed login stands ready (`NoFollow`: it continues on the dead one). The login
            // back in another tab is this tab's lift whoever's `/login` this loss had — its own
            // or a person's, left or standing (`NoCarrierBack`: only a tab that typed none is
            // delivered). Answered by the model, the login is back, the track ends and the
            // notices are read; answered by the wall, its row and the transcript's word.
            action Continue when (
                (wall == 0 || (elsewhere == 2 && track == 0) || Buggy == 2 || NoSee == 1) &&
                ((track == 0 && left == 0) || back == 1 ||
                    (elsewhere == 2 && (Buggy <= 5 || Buggy > 6) && NoCarrierBack == 0) ||
                    Buggy == 2 || NoHold == 1) &&
                (renewed == 0 || Buggy == 3 || NoFollow == 1)
            ) {
                futile = if (login == 0 && stood == 1) { 1 } else { futile };
                stranded = if (renewed == 1 && login == 0) { 1 } else { stranded };
                untold = if (stood == 1 && told == 0) { 1 } else { untold };
                wall = if (login == 1) { 0 } else { 1 };
                stood = if (login == 1) { 0 } else { 1 };
                back = 0;
                done = 0;
                picker = if (login == 1) { 0 } else { picker };
                track = if (login == 1) { 0 } else { track };
                left = if (login == 1) { 0 } else { left };
                told = if (login == 1) { 0 } else { told };
                elsewhere = if (login == 1) { 0 } else { elsewhere };
                holding = if (login == 1 && (phase == 1 || phase == 2)) { 1 } else { holding };
                // A login back that only another tab's screen said: this
                // turn the model answers is its lift, as `Login successful`
                // is ([`LoginBack`]).
                window = if (
                    login == 1 && stood == 1 && phase == 1 && (Buggy == 0 || Buggy > 1) &&
                    ClockAtWall == 0
                ) {
                    0
                } else {
                    window
                };
                dark = if (login == 0 && phase == 1 && window == 0) {
                    1
                } else if (login == 1 && stood == 1 && (Buggy == 0 || Buggy > 1) && ClockAtWall == 0) {
                    0
                } else {
                    dark
                };
            }

            // The upgrade. A notice is never typed where the wall shows or stands (`NoGate`: it
            // is); a notice the wall answered is typed again as the same ask, and a give-up
            // spent on such notices is taken back (`NoRefund`: neither). Typed with the login
            // gone, its own turn is the wall's.
            action Announce when (
                ready == 0 &&
                ((wall == 0 && stood == 0) || Buggy == 1 || NoGate == 1) &&
                (phase == 0 ||
                    (phase == 1 &&
                        ((unread == 1 && (Buggy == 0 || Buggy > 1) && NoRefund == 0) ||
                            (window == 1 && asks <= MaxAsks - 1))) ||
                    (phase == 2 && unread == 1 && (Buggy == 0 || Buggy > 1) && NoRefund == 0))
            ) {
                blind = if (wall == 1 || stood == 1) { 1 } else { blind };
                asks = if (
                    phase == 1 && unread == 1 && (Buggy == 0 || Buggy > 1) && NoRefund == 0
                ) {
                    asks
                } else if (phase == 2) {
                    got + 1
                } else {
                    asks + 1
                };
                got = if (login == 1) { got + 1 } else { got };
                unread = if (login == 1) { 0 } else { 1 };
                holding = if (login == 1) { 1 } else { holding };
                wall = if (login == 1) { wall } else { 1 };
                stood = if (login == 1) { stood } else { 1 };
                back = if (login == 1) { back } else { 0 };
                phase = 1;
                window = 0;
                dark = 0;
            }
            // The re-ask window runs only where the agent can answer.
            action Elapse when (
                phase == 1 && window == 0 &&
                ((wall == 0 && stood == 0) || Buggy == 1 || ClockAtWall == 1)
            ) {
                window = 1;
            }
            action GiveUp when (
                phase == 1 && window == 1 && asks == MaxAsks && ready == 0 &&
                ((wall == 0 && stood == 0) || Buggy == 1 || NoGate == 1) &&
                (unread == 0 || Buggy == 1 || NoRefund == 1)
            ) {
                phase = 2;
                spent = if (got <= MaxAsks - 1 || dark == 1) { 1 } else { spent };
            }
            // The agent answers READY (its own turn: the login must hold) and the restart acts
            // on it, an announced upgrade's or a late answer to one that gave up, never into
            // the wall.
            action AgentReady when (
                holding == 1 && login == 1 && ready == 0 && (phase == 1 || phase == 2)
            ) {
                ready = 1;
            }
            action Restart when (
                ready == 1 && (phase == 1 || phase == 2) &&
                ((wall == 0 && stood == 0) || Buggy == 1 || NoGate == 1)
            ) {
                blind = if (wall == 1 || stood == 1) { 1 } else { blind };
                phase = 3;
                ready = 0;
                holding = 0;
            }

            invariant NoNoticeAtTheWall: blind == 0;
            invariant NoGiveUpUnread: spent == 0;
            invariant NoFutileContinue: futile == 0;
            invariant TheOwnerIsToldFirst: untold == 0;
            invariant NoSessionLeftOnADeadLogin: stranded == 0;
            invariant NoCheckUnderAnIdleLogin: checked == 0;
            invariant NoLostLoginLeftSilent: silent == 0;
            invariant LoginTypedOncePerLoss: relooped == 0;
            invariant NoMethodGuessed: guessed == 0;
            invariant NoLoginOverAPersonsCancel: overrode == 0;
            invariant NoPersonsPickerAnswered: overpicked == 0;
            invariant OneSignInPerLogin: doubled == 0;
            invariant NoSignInAskedOfALoginBack: misled == 0;
            invariant NoSignInAskedOverARenewal: reasked == 0;
        }
    }
}

/// The usage tap (aterm-agent's `harness::tap`; the accounts design v2 §3.1, §3.2): one tab's
/// Claude Code across aterm's relaunches, the status-line tap aterm may put on the line, the
/// readings it writes, and the host's read of them at its decided turn ends.
///
/// The owner's and machine's facts move freely: `armed` (`[harness] usage_tap`), `userline` (the
/// session's own statusLine or `--settings`), `cred` (the process uses a stored login of the
/// machine's one account, the default login or the one a renewal bound its row to, and no roster
/// lists a second account, which is what attributes a reading). A relaunch (`Launch`, a new
/// `pid`) carries the tap only where its gate holds (`tapped`; `NoChainLook` ignores the owner's
/// line). The process serves a request (`Reply`: `api` rises, a turn is `served`), moves its
/// figure on the refresh timer (`BgUsage`), or re-reports (`IdleReport`); each run of a tapped
/// process leaves a `file`. The host reads it (`Read`), fresh only as §3.2 says (another process
/// than the last admitted, `lpid`; more API time, `lapi`; another figure, `lfigs`; `NoFresh`
/// takes every reading), and records the figure (`noted`, the max rule) only for the machine's
/// account (`NoAttrib`: any). A decided point 30 s on (`Tick30`) ages the file and strikes a
/// tapped process whose file is stale; two strikes mark it `silent`, only for one that served a
/// turn (`NoServeGate`: any), and the window is told (`said`) until the mark ends: the process
/// heard again, or another launched in its place (`NoEndAtRelaunch`: the mark outlives its
/// process). Ghosts: `stale` (a reading recorded that was no news), `displaced` (a tap put over
/// the owner's line), `unattrib` (a reading recorded for a process not on the machine's account).
///
/// Properties: `NeverNotesStale`, `NeverArmsOverTheOwnersLine`, `NoTapOnlyAfterServe`,
/// `NothingNotedUnattributed`, `NoMarkOutlivesItsProcess`; one knob per defect, `Buggy = 1` all
/// five. Tier-1: aterm-agent's `conformance_usage_tap`, over the real `tap::gate`,
/// `tap::Fresh::admit`, `tap::Silence::step`, `tap::Silence::untapped` and
/// `accounts::note_reading` on every reachable state.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_usage_tap_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUsageTap {
            const Buggy = 0;
            const NoFresh = 0;
            const NoChainLook = 0;
            const NoServeGate = 0;
            const NoAttrib = 0;
            const NoEndAtRelaunch = 0;
            const MaxPid = 2;
            var armed = 1;
            var userline = 0;
            var cred = 1;
            var tapped = 0;
            var pid = 0;
            var api = 0;
            var figs = 0;
            var file = 0;
            var lpid = 0;
            var lapi = 0;
            var lfigs = 0;
            var noted = 0;
            var served = 0;
            var strike = 0;
            var silent = 0;
            var said = 0;
            var stale = 0;
            var displaced = 0;
            var unattrib = 0;

            // The owner's and the machine's facts, as they please.
            action Switch when (MaxPid > pid) {
                armed = 1 - armed;
            }
            action OwnLine when (MaxPid > pid) {
                userline = 1 - userline;
            }
            action Credential when (pid > 0) {
                cred = 1 - cred;
            }

            // aterm types a relaunch line: the tap where its gate holds.
            action Launch when (MaxPid > pid) {
                pid = pid + 1;
                tapped = if (
                    armed == 1 && cred == 1 &&
                    (userline == 0 || Buggy == 1 || NoChainLook == 1)
                ) { 1 } else { 0 };
                displaced = if (
                    armed == 1 && cred == 1 && userline == 1 &&
                    (Buggy == 1 || NoChainLook == 1)
                ) { 1 } else { displaced };
                api = 0;
                file = 0;
                served = 0;
                strike = 0;
                silent = 0;
                said = if (Buggy == 1 || NoEndAtRelaunch == 1) { said } else { 0 };
            }

            // The process: a request served, a figure moved on the timer, a
            // re-report of what it holds. A tapped one writes each time.
            action Reply when (pid > 0 && 1 > api) {
                api = api + 1;
                served = 1;
                file = tapped;
            }
            action BgUsage when (pid > 0 && 1 > figs) {
                figs = figs + 1;
                file = tapped;
            }
            action IdleReport when (pid > 0) {
                file = tapped;
            }

            // The host at a decided turn end: the file, fresh or not, and
            // the record of a fresh one for the machine's own login.
            action Read when (pid > 0 && file == 1) {
                noted = if (
                    (pid > lpid || lpid > pid || api > lapi || figs > lfigs || lfigs > figs ||
                        Buggy == 1 || NoFresh == 1) &&
                    (cred == 1 || Buggy == 1 || NoAttrib == 1) &&
                    figs > noted
                ) { figs } else { noted };
                stale = if (
                    (Buggy == 1 || NoFresh == 1) &&
                    (cred == 1 || Buggy == 1 || NoAttrib == 1) &&
                    pid == lpid && api <= lapi && figs == lfigs
                ) { 1 } else { stale };
                unattrib = if (
                    (pid > lpid || lpid > pid || api > lapi || figs > lfigs || lfigs > figs ||
                        Buggy == 1 || NoFresh == 1) &&
                    (Buggy == 1 || NoAttrib == 1) && cred == 0
                ) { 1 } else { unattrib };
                lpid = if (
                    pid > lpid || lpid > pid || api > lapi || figs > lfigs || lfigs > figs ||
                        Buggy == 1 || NoFresh == 1
                ) { pid } else { lpid };
                lapi = if (
                    pid > lpid || lpid > pid || api > lapi || figs > lfigs || lfigs > figs ||
                        Buggy == 1 || NoFresh == 1
                ) { api } else { lapi };
                lfigs = if (
                    pid > lpid || lpid > pid || api > lapi || figs > lfigs || lfigs > figs ||
                        Buggy == 1 || NoFresh == 1
                ) { figs } else { lfigs };
            }

            // A decided point 30 s on: the file ages; a tapped process whose
            // file is stale is struck, and silent at the second strike.
            action Tick30 when (pid > 0) {
                strike = if (file == 1) {
                    0
                } else if (
                    tapped == 1 && (served == 1 || Buggy == 1 || NoServeGate == 1) && 1 > strike
                ) {
                    strike + 1
                } else {
                    strike
                };
                silent = if (file == 1) {
                    0
                } else if (
                    tapped == 1 && (served == 1 || Buggy == 1 || NoServeGate == 1) && strike == 1
                ) {
                    1
                } else {
                    silent
                };
                said = if (file == 1) {
                    0
                } else if (
                    tapped == 1 && (served == 1 || Buggy == 1 || NoServeGate == 1) && strike == 1
                ) {
                    1
                } else {
                    said
                };
                file = 0;
            }

            invariant NeverNotesStale: stale == 0;
            invariant NeverArmsOverTheOwnersLine: displaced == 0;
            invariant NoTapOnlyAfterServe: silent == 0 || served == 1;
            invariant NothingNotedUnattributed: unattrib == 0;
            invariant NoMarkOutlivesItsProcess: said == 0 || silent == 1;
        }
    }
}

/// The notice queued behind a usage limit (`aterm_agent::harness::upgrade`: `notice_scan`,
/// `queued_until`, `queued_copies`, `Scan::rests_until`, `queue_rest`, `transcript_limit_until`,
/// `next_step`, `requested_step`, `announce_asks`, `clock_held`; the driver's `queue_facts`). A
/// session at its usage limit answers every notice with its limit row; read as asked, the
/// notices pile up, the round gives up on asks the model never had, and when the session goes
/// on they all reach the agent at once.
///
/// The account: `limited` (at its usage limit; no reader sees this directly). What the upgrade
/// reads: `shown` (the screen shows the limit; a person's Esc or a missed recognition clears it
/// while the limit stands) and `held` (the transcript's word that the limit holds: the limit row
/// that last answered the latest notice, or with nothing queued the session's last row, its
/// named reset ahead or, naming none, written less than `REASK_S` ago). The conversation:
/// `untaken` (notices the model has not taken, each answered by the limit; they reach the model
/// together with the next turn that gets past it), counted to `RetypeMax + 1 + Daily`, meaning
/// that many or more (a queue is full past `RetypeMax`, and its rest is a day at the top);
/// `took` (asks the model took). The upgrade: `phase` 0 pending, 1 announced, 2 gave up; `asks`
/// counted to `MaxAsks`; `window` (the latest ask's re-ask window has run out); `rested` (a full
/// queue has rested since the transcript's word on its latest notice ran out: 1 under a day, 2 a
/// day). Ghosts: `stacked` (a fresh ask typed while a notice waited untaken), `burst` (a notice
/// typed into a full queue before it rested), `hurried` (notices typed into a full queue, the
/// limit standing, without a day's rest before them; saturating at `Daily + 1`), `spent` (a
/// give-up before the model took `MaxAsks`), `stuck` (the quiet word while the upgrade still
/// owed the agent a readable ask).
///
/// `Retype` types the same ask again when its latest copy was queued and the limit is over by the
/// transcript's word and the screen's (a named reset passed, a `/login` finished, `REASK_S` since
/// a row naming none), which for a row naming no reset is every half hour however long the limit
/// stands. So it is bounded: at once only while at most `RetypeMax` (`REQUEUE_MAX`) wait
/// untaken; into a full queue only once it has rested (`Rests`, the real `queue_rest`), one copy
/// per rest, and the rest grows with every unread copy, since a fixed rest under a days-long
/// limit naming no reset still adds about ten copies a day (two, four, eight, sixteen hours for
/// the first `Daily`, `QUEUE_REST_DOUBLINGS`; a day after). `NowAsks` is the owner's `Upgrade now`
/// on a full queue. `GoesOn` is a turn someone else began getting past the limit. `Look` is the
/// upgrade's quiet word where the limit is over by every word, derived from its own guards:
/// enabled exactly where none of `Announce`, `Retype`, `GiveUp`, `Elapse` and `Rests` is, which
/// the Tier-0 test checks state by state. One round is modelled: a stopped round's rest and new
/// round, the READY and the restart are `HarnessUpgradeNeverStrands`'. A notice an earlier round,
/// target or build left queued is read by the real code as this queue is; that crossing of rounds
/// is not modelled.
///
/// Properties: `NeverTwoAsksUntaken` (no ask typed behind an untaken notice; one ask's copies
/// are the retype's); `CopiesBounded` (at most `RetypeMax + 1` untaken before a rest, then one
/// per rest); `CopiesPerDayBounded` (at most `Daily` notices into a full queue without a day's
/// rest, so a limit lasting days adds `1 + RetypeMax + Daily` at most before its rests reach a
/// day, then one a day); `NoGiveUpUntaken` (a give-up only after the model took `MaxAsks`);
/// `NeverStranded` (once the limit is over by every word, an announced round always has a next
/// act of its own and never gives up before the model took `MaxAsks`).
///
/// One knob per defect, each caught alone: `CountRetype` (a retype spends an ask and is given up
/// on at the bound: `NoGiveUpUntaken`), `Unbounded` (a retype every time the transcript says the
/// limit is over: `CopiesBounded`), `NoBound` (a queued notice holds the upgrade until the model
/// takes it: `NeverStranded`), `ForGood` (a full queue never rests: `NeverStranded`), `Flat`
/// (every rest as long as the first: `CopiesPerDayBounded`). `Buggy = 1` composes the screen-only
/// limit read, notices answered by the limit read as asked, a window run from the typing, and the
/// flat rest, so the ratchet's `Buggy = 1` walk sees every invariant caught.
///
/// Tier-1: aterm-agent's `harness::upgrade_drive` tests (`upgrade_queued_tests.rs`), over the
/// real screen reader, transcript readers, `queue_facts`, clock, reducer (with and without the
/// owner's `--now`), ask count and rest on every reachable state and several transcripts each.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_limit_queue_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeLimitQueue {
            const Buggy = 0;
            const CountRetype = 0;
            const Unbounded = 0;
            const NoBound = 0;
            const ForGood = 0;
            const Flat = 0;
            const MaxAsks = 4;
            const RetypeMax = 2;
            const Daily = 4;
            var limited = 0;
            var shown = 0;
            var held = 0;
            var phase = 0;
            var asks = 0;
            var window = 0;
            var untaken = 0;
            var took = 0;
            var rested = 0;
            var stacked = 0;
            var burst = 0;
            var hurried = 0;
            var spent = 0;
            var stuck = 0;

            // The account and the session, as they please. The agent's own turn meets the
            // limit: the screen shows it and its row holds the transcript's word, until the
            // reset it names or, naming none, for `REASK_S`; an announced clock is held.
            action LimitHits when (limited == 0 && untaken == 0 && phase <= 1) {
                limited = 1;
                shown = 1;
                held = 1;
                window = 0;
            }
            action LimitEnds when (limited == 1) {
                limited = 0;
            }
            // The transcript's word runs out (the named reset passes, a `/login` finishes, or
            // `REASK_S` since a row naming none), whether or not the limit ended.
            action BoundPasses when (held == 1) {
                held = 0;
            }
            // The banner leaves the screen (a person's Esc, a recogniser that misses it),
            // whether or not the limit ended.
            action Clear when (shown == 1) {
                shown = 0;
            }
            // A turn someone else began gets past the limit (Claude Code's own at the reset, a
            // person's, a peer's): the model takes every waiting notice at once, one ask, and
            // the latest ask's window opens then (`Buggy`: it ran from the typing).
            action GoesOn when (limited == 0 && untaken > 0) {
                untaken = 0;
                took = took + 1;
                shown = 0;
                held = 0;
                rested = 0;
                hurried = 0;
                window = if Buggy == 1 { window } else { 0 };
            }
            // The owner's `Upgrade now` on a full queue: one copy more, never where the screen
            // shows the limit or the transcript's word holds it; read, or queued one more
            // unread (lengthening the next rest).
            action NowAsks when (
                Buggy == 0 && phase == 1 && untaken > RetypeMax && shown == 0 && held == 0
            ) {
                window = 0;
                took = if limited == 0 { took + 1 } else { took };
                untaken = if limited == 1 {
                    if untaken > RetypeMax + Daily { untaken } else { untaken + 1 }
                } else { 0 };
                hurried = if limited == 1 { hurried } else { 0 };
                shown = limited;
                held = limited;
                rested = 0;
            }

            // The upgrade reads a limit where the screen shows one or the transcript's word
            // holds, and a full queue (more than `RetypeMax` untaken) until it has rested
            // (`Unbounded`: never full; `NoBound`: any queue holds; `Buggy`: the screen alone).
            // A fresh ask: the first notice, or the next once the window of one the model took
            // has run out, never behind an untaken notice (`Buggy`: a queued notice read as
            // asked). The model reads it, or, at the limit, the limit answers and it queues.
            action Announce when (
                shown == 0 &&
                (Buggy == 1 ||
                    (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                        (untaken == 0 || NoBound == 0))) &&
                (untaken == 0 || Buggy == 1) &&
                (phase == 0 || (phase == 1 && window == 1 && asks <= MaxAsks - 1))
            ) {
                stacked = if untaken > 0 { 1 } else { stacked };
                burst = if untaken > RetypeMax && rested == 0 { 1 } else { burst };
                hurried = if (limited == 1 && untaken > RetypeMax && rested <= 1) {
                    if hurried > Daily { hurried } else { hurried + 1 }
                } else if (limited == 1) { hurried } else { 0 };
                phase = 1;
                asks = asks + 1;
                window = 0;
                took = if limited == 0 { took + 1 } else { took };
                untaken = if limited == 1 {
                    if untaken > RetypeMax + Daily { untaken } else { untaken + 1 }
                } else { 0 };
                shown = limited;
                held = limited;
                rested = 0;
            }
            // The same ask again: its latest copy was the limit's and that limit is over by the
            // transcript's word and the screen's; at once while the queue has room, into a full
            // one only once it has rested (`Unbounded`: no bound; `ForGood`: a full queue never;
            // `CountRetype`: it spends an ask; `Buggy`: only into a full queue that rested).
            action Retype when (
                phase == 1 && untaken > 0 && shown == 0 && held == 0 &&
                (Buggy == 0 || (untaken > RetypeMax && rested > 0)) &&
                (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                NoBound == 0 &&
                (CountRetype == 0 || asks <= MaxAsks - 1)
            ) {
                burst = if untaken > RetypeMax && rested == 0 { 1 } else { burst };
                hurried = if (limited == 1 && untaken > RetypeMax && rested <= 1) {
                    if hurried > Daily { hurried } else { hurried + 1 }
                } else if (limited == 1) { hurried } else { 0 };
                asks = if CountRetype == 1 { asks + 1 } else { asks };
                window = 0;
                took = if limited == 0 { took + 1 } else { took };
                untaken = if limited == 1 {
                    if untaken > RetypeMax + Daily { untaken } else { untaken + 1 }
                } else { 0 };
                shown = limited;
                held = limited;
                rested = 0;
            }
            // A full queue rests after the transcript's word on its latest notice runs out:
            // shorter than a day while it holds at most `RetypeMax + Daily` unread, a day once
            // it holds more (`Flat`, `Buggy`: every rest the first one's).
            action Rests when (phase == 1 && untaken > RetypeMax && held == 0 && rested == 0) {
                rested = if (Flat == 0 && Buggy == 0 && untaken > RetypeMax + Daily) { 2 } else { 1 };
            }
            // The re-ask window runs only where the upgrade reads no limit
            // and no full queue.
            action Elapse when (
                phase == 1 && window == 0 && shown == 0 &&
                (Buggy == 1 ||
                    (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                        (untaken == 0 || NoBound == 0)))
            ) {
                window = 1;
            }
            // Its asks spent, the last one's window run out and nothing untaken: it gives up
            // (`CountRetype`: a queued notice's re-type at the bound is the give-up).
            action GiveUp when (
                phase == 1 && asks == MaxAsks && shown == 0 &&
                (Buggy == 1 ||
                    (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                        (untaken == 0 || NoBound == 0))) &&
                ((window == 1 && (untaken == 0 || Buggy == 1)) ||
                    (CountRetype == 1 && Buggy == 0 && untaken > 0))
            ) {
                phase = 2;
                spent = if took <= MaxAsks - 1 { 1 } else { spent };
            }
            // The quiet word where the limit is over by every word (the account's, the
            // screen's, the transcript's): its own guards negated. A strand is an announced
            // round with nothing of its own left to do, or a round given up before the model
            // took `MaxAsks`.
            action Look when (
                phase > 0 && limited == 0 && shown == 0 && held == 0 && stuck == 0 &&
                (if (
                    shown == 0 &&
                    (Buggy == 1 ||
                        (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                            (untaken == 0 || NoBound == 0))) &&
                    (untaken == 0 || Buggy == 1) &&
                    (phase == 0 || (phase == 1 && window == 1 && asks <= MaxAsks - 1))
                ) { 1 } else { 0 }) +
                (if (
                    phase == 1 && untaken > 0 && shown == 0 && held == 0 &&
                    (Buggy == 0 || (untaken > RetypeMax && rested > 0)) &&
                    (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                    NoBound == 0 &&
                    (CountRetype == 0 || asks <= MaxAsks - 1)
                ) { 1 } else { 0 }) +
                (if (
                    phase == 1 && untaken > RetypeMax && held == 0 && rested == 0
                ) { 1 } else { 0 }) +
                (if (
                    phase == 1 && window == 0 && shown == 0 &&
                    (Buggy == 1 ||
                        (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                            (untaken == 0 || NoBound == 0)))
                ) { 1 } else { 0 }) +
                (if (
                    phase == 1 && asks == MaxAsks && shown == 0 &&
                    (Buggy == 1 ||
                        (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                            (untaken == 0 || NoBound == 0))) &&
                    ((window == 1 && (untaken == 0 || Buggy == 1)) ||
                        (CountRetype == 1 && Buggy == 0 && untaken > 0))
                ) { 1 } else { 0 }) == 0
            ) {
                stuck = if (phase == 1 || (phase == 2 && took <= MaxAsks - 1)) { 1 } else { 0 };
            }

            invariant NeverTwoAsksUntaken: stacked == 0;
            invariant CopiesBounded: burst == 0;
            invariant CopiesPerDayBounded: hurried <= Daily;
            invariant NoGiveUpUntaken: spent == 0;
            invariant NeverStranded: stuck == 0;
        }
    }
}

/// The model priority list (`harness::upgrade_models`): a writer/reader pair, the shape of
/// `NativeUpdateFailedMarkSuppression`. The writers are `Priority::admit` (automatic insertion of
/// a model Claude Code recommends) and the owner's `models set`; the reader is target selection
/// (`upgrade_models::target`, read once per sweep), which relaunches a session onto the first
/// available entry. A writer's entry means to the reader exactly what the owner's order meant,
/// and nothing the owner did not ask for.
///
/// The list maps id to position, best first, `0` absent. The owner's three are `a1` =
/// `claude-opus-5`, `a2` = `claude-opus-5-5` and `b1` = `claude-fable-5-1` (always listed; `hord`
/// names which of three owner orders was last set, `0` the seed). A recommendation may bring `a3`
/// = `claude-opus-5-6` and `b2` = `claude-fable-5-2`; three must never land: `a0` =
/// `claude-opus-4-8` (older than every listed Opus), `ah` = `claude-opus-5-1` (between
/// `claude-opus-5` and `claude-opus-5-5`) and `c1` = `claude-sonnet-6` (an unlisted family).
/// `k_a3` / `k_b2` are the build's catalog knowing the newcomers (knowledge only grows); `av_*`
/// is each id's availability; `pick` is the reader's position, valid while `sel == 1` and dropped
/// by every change. Tier-1 drives the real `admit`, `render`/`parse` and `target` over every
/// reachable state and projects their answers onto these variables.
///
/// `Buggy = 1` arms one mutant per law, each changing nothing on its healthy branch (where the
/// real `admit` answers `None`): a downgrade (`OfferDowngrade`), a newest check against only the
/// family's best-ranked member, which extends a hand-ordered list downward (`OfferBelowNewest`),
/// an unlisted family (`OfferCrossFamily`), an id the build does not know (`AdmitUnknown`), an
/// insertion at the head instead of above the family's best (`AdmitAtTheTop`), an insertion that
/// re-sorts the family newest-first and rewrites the owner's order (`AdmitAndResort`), and a
/// reader that answers the head whether or not it is available (`SelectHead`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_model_priority_model() -> Model {
    aterm_spec::ty_model! {
        HarnessModelPriority {
            const Buggy = 0;

            // The seed order: claude-opus-5-5, claude-fable-5-1, claude-opus-5.
            var p_a1 = 3;
            var p_a2 = 1;
            var p_b1 = 2;
            var p_a3 = 0;
            var p_b2 = 0;
            var p_a0 = 0;
            var p_ah = 0;
            var p_c1 = 0;
            var hord = 0;
            var k_a3 = 0;
            var k_b2 = 0;
            var av_a1 = 0;
            var av_a2 = 0;
            var av_a3 = 0;
            var av_b1 = 0;
            var av_b2 = 0;
            var sel = 0;
            var pick = 0;
            var forged = 0;

            // -- the environment ------------------------------------------

            action BuildLearnsNewerOpus when (k_a3 == 0) {
                k_a3 = 1;
            }
            action BuildLearnsNewerFable when (k_b2 == 0) {
                k_b2 = 1;
            }
            action ToggleA1 {
                av_a1 = 1 - av_a1;
                sel = 0;
                pick = 0;
            }
            action ToggleA2 {
                av_a2 = 1 - av_a2;
                sel = 0;
                pick = 0;
            }
            action ToggleA3 {
                av_a3 = 1 - av_a3;
                sel = 0;
                pick = 0;
            }
            action ToggleB1 {
                av_b1 = 1 - av_b1;
                sel = 0;
                pick = 0;
            }
            action ToggleB2 {
                av_b2 = 1 - av_b2;
                sel = 0;
                pick = 0;
            }

            // -- the owner (`models set`): the whole list, verbatim --------

            // The seed order again: claude-opus-5-5, claude-fable-5-1, claude-opus-5.
            action HumanSetsTheSeed {
                p_a1 = 3;
                p_a2 = 1;
                p_b1 = 2;
                p_a3 = 0;
                p_b2 = 0;
                p_a0 = 0;
                p_ah = 0;
                p_c1 = 0;
                hord = 0;
                sel = 0;
                pick = 0;
            }
            // The older Opus ranked first, the order the newest-listed check exists for.
            action HumanSetsOlderOpusFirst {
                p_a1 = 1;
                p_a2 = 2;
                p_b1 = 3;
                p_a3 = 0;
                p_b2 = 0;
                p_a0 = 0;
                p_ah = 0;
                p_c1 = 0;
                hord = 1;
                sel = 0;
                pick = 0;
            }
            // Another family ranked above every Opus.
            action HumanSetsFableFirst {
                p_a1 = 2;
                p_a2 = 3;
                p_b1 = 1;
                p_a3 = 0;
                p_b2 = 0;
                p_a0 = 0;
                p_ah = 0;
                p_c1 = 0;
                hord = 2;
                sel = 0;
                pick = 0;
            }

            // -- the automatic writer (`Priority::admit`) --------------------

            // Strictly newer than every listed Opus and known to the build: directly above the
            // family's best-ranked member.
            action AdmitNewerOpus when (p_a3 == 0 && k_a3 == 1) {
                p_a3 = (if p_a1 <= p_a2 { p_a1 } else { p_a2 });
                p_a1 = if (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_b2 = if p_b2 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_a0 = if p_a0 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_ah = if p_ah > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_ah { p_ah + 1 } else { p_ah };
                p_c1 = if p_c1 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = 0;
                pick = 0;
            }
            action AdmitNewerFable when (p_b2 == 0 && k_b2 == 1) {
                p_b2 = p_b1;
                p_a1 = if p_b1 <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if p_b1 <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if p_b1 <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_a3 = if p_a3 > 0 && p_b1 <= p_a3 { p_a3 + 1 } else { p_a3 };
                p_a0 = if p_a0 > 0 && p_b1 <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_ah = if p_ah > 0 && p_b1 <= p_ah { p_ah + 1 } else { p_ah };
                p_c1 = if p_c1 > 0 && p_b1 <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = 0;
                pick = 0;
            }

            // -- the reader (`upgrade_models::target`): the first available entry ----------
            action Select when (sel == 0) {
                pick = if (p_a1 == 1 && av_a1 == 1) ||
                    (p_a2 == 1 && av_a2 == 1) ||
                    (p_a3 == 1 && av_a3 == 1) ||
                    (p_b1 == 1 && av_b1 == 1) ||
                    (p_b2 == 1 && av_b2 == 1) {
                    1
                } else if (p_a1 == 2 && av_a1 == 1) ||
                    (p_a2 == 2 && av_a2 == 1) ||
                    (p_a3 == 2 && av_a3 == 1) ||
                    (p_b1 == 2 && av_b1 == 1) ||
                    (p_b2 == 2 && av_b2 == 1) {
                    2
                } else if (p_a1 == 3 && av_a1 == 1) ||
                    (p_a2 == 3 && av_a2 == 1) ||
                    (p_a3 == 3 && av_a3 == 1) ||
                    (p_b1 == 3 && av_b1 == 1) ||
                    (p_b2 == 3 && av_b2 == 1) {
                    3
                } else if (p_a1 == 4 && av_a1 == 1) ||
                    (p_a2 == 4 && av_a2 == 1) ||
                    (p_a3 == 4 && av_a3 == 1) ||
                    (p_b1 == 4 && av_b1 == 1) ||
                    (p_b2 == 4 && av_b2 == 1) {
                    4
                } else if (p_a1 == 5 && av_a1 == 1) ||
                    (p_a2 == 5 && av_a2 == 1) ||
                    (p_a3 == 5 && av_a3 == 1) ||
                    (p_b1 == 5 && av_b1 == 1) ||
                    (p_b2 == 5 && av_b2 == 1) {
                    5
                } else {
                    0
                };
                sel = 1;
            }

            // -- the defects; every healthy branch changes nothing ----------

            action OfferDowngrade when (p_a0 == 0 && forged == 0) {
                p_a0 = if Buggy == 1 { (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) } else { p_a0 };
                p_a1 = if Buggy == 1 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if Buggy == 1 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if Buggy == 1 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_a3 = if Buggy == 1 && p_a3 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a3 { p_a3 + 1 } else { p_a3 };
                p_b2 = if Buggy == 1 && p_b2 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_ah = if Buggy == 1 && p_ah > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_ah { p_ah + 1 } else { p_ah };
                p_c1 = if Buggy == 1 && p_c1 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = if Buggy == 1 { 0 } else { sel };
                pick = if Buggy == 1 { 0 } else { pick };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // Newer than the best-ranked Opus only.
            action OfferBelowNewest when (p_ah == 0 && p_a1 <= p_a2 && forged == 0) {
                p_ah = if Buggy == 1 { p_a1 } else { p_ah };
                p_a1 = if Buggy == 1 && p_a1 <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if Buggy == 1 && p_a1 <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if Buggy == 1 && p_a1 <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_a3 = if Buggy == 1 && p_a3 > 0 && p_a1 <= p_a3 { p_a3 + 1 } else { p_a3 };
                p_b2 = if Buggy == 1 && p_b2 > 0 && p_a1 <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_a0 = if Buggy == 1 && p_a0 > 0 && p_a1 <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_c1 = if Buggy == 1 && p_c1 > 0 && p_a1 <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = if Buggy == 1 { 0 } else { sel };
                pick = if Buggy == 1 { 0 } else { pick };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action OfferCrossFamily when (p_c1 == 0 && forged == 0) {
                p_c1 = if Buggy == 1 { 1 } else { p_c1 };
                p_a1 = if Buggy == 1 && 1 <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if Buggy == 1 && 1 <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if Buggy == 1 && 1 <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_a3 = if Buggy == 1 && p_a3 > 0 && 1 <= p_a3 { p_a3 + 1 } else { p_a3 };
                p_b2 = if Buggy == 1 && p_b2 > 0 && 1 <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_a0 = if Buggy == 1 && p_a0 > 0 && 1 <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_ah = if Buggy == 1 && p_ah > 0 && 1 <= p_ah { p_ah + 1 } else { p_ah };
                sel = if Buggy == 1 { 0 } else { sel };
                pick = if Buggy == 1 { 0 } else { pick };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action AdmitUnknown when (p_a3 == 0 && k_a3 == 0 && forged == 0) {
                p_a3 = if Buggy == 1 { (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) } else { p_a3 };
                p_a1 = if Buggy == 1 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if Buggy == 1 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if Buggy == 1 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_b2 = if Buggy == 1 && p_b2 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_a0 = if Buggy == 1 && p_a0 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_ah = if Buggy == 1 && p_ah > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_ah { p_ah + 1 } else { p_ah };
                p_c1 = if Buggy == 1 && p_c1 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = if Buggy == 1 { 0 } else { sel };
                pick = if Buggy == 1 { 0 } else { pick };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action AdmitAtTheTop when (p_a3 == 0 && k_a3 == 1 && forged == 0) {
                p_a3 = if Buggy == 1 { 1 } else { p_a3 };
                p_a1 = if Buggy == 1 && 1 <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if Buggy == 1 && 1 <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if Buggy == 1 && 1 <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_b2 = if Buggy == 1 && p_b2 > 0 && 1 <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_a0 = if Buggy == 1 && p_a0 > 0 && 1 <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_ah = if Buggy == 1 && p_ah > 0 && 1 <= p_ah { p_ah + 1 } else { p_ah };
                p_c1 = if Buggy == 1 && p_c1 > 0 && 1 <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = if Buggy == 1 { 0 } else { sel };
                pick = if Buggy == 1 { 0 } else { pick };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // Insert above the best and sort the family newest-first.
            action AdmitAndResort when (
                p_a3 == 0 && k_a3 == 1 && p_a1 <= p_a2 && forged == 0
            ) {
                p_a3 = if Buggy == 1 { p_a1 } else { p_a3 };
                p_a2 = if Buggy == 1 { p_a1 + 1 } else { p_a2 };
                p_a1 = if Buggy == 1 { p_a2 + 1 } else { p_a1 };
                p_b1 = if Buggy == 1 && p_a1 <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_b2 = if Buggy == 1 && p_b2 > 0 && p_a1 <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_a0 = if Buggy == 1 && p_a0 > 0 && p_a1 <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_ah = if Buggy == 1 && p_ah > 0 && p_a1 <= p_ah { p_ah + 1 } else { p_ah };
                p_c1 = if Buggy == 1 && p_c1 > 0 && p_a1 <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = if Buggy == 1 { 0 } else { sel };
                pick = if Buggy == 1 { 0 } else { pick };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action SelectHead when (sel == 0 && forged == 0) {
                pick = if Buggy == 1 { 1 } else { pick };
                sel = if Buggy == 1 { 1 } else { sel };
                forged = if Buggy == 1 { 1 } else { forged };
            }

            // Nothing older than the family's best-ranked member is admitted.
            invariant AutoInsertIsNewerThanTheFamilysBest: p_a0 == 0;
            // Nor older than the family's newest listed member: a hand-ordered list is never
            // extended downward.
            invariant NeverExtendedDownward: p_ah == 0;
            invariant AutoInsertIsSameFamily: p_c1 == 0;
            invariant AutoInsertIsKnownToTheBuild:
                (p_a3 == 0 || k_a3 == 1) && (p_b2 == 0 || k_b2 == 1);
            invariant AutoInsertSitsDirectlyAboveTheFamilysBest:
                (p_a3 == 0 || p_a3 + 1 == (if p_a1 <= p_a2 { p_a1 } else { p_a2 })) &&
                (p_b2 == 0 || p_b2 + 1 == p_b1);
            // The owner's three keep the owner's order, whatever was inserted.
            invariant HumanOrderIsNeverRewritten:
                if hord == 0 {
                    p_a2 + 1 <= p_b1 && p_b1 + 1 <= p_a1
                } else if hord == 1 {
                    p_a1 + 1 <= p_a2 && p_a2 + 1 <= p_b1
                } else {
                    p_b1 + 1 <= p_a1 && p_a1 + 1 <= p_a2
                };
            // The pick is available and nothing available ranks above it.
            invariant ReaderPicksTheFirstAvailable:
                sel == 0 || (
                    (pick == 0 ||
                        (p_a1 == pick && av_a1 == 1) ||
                        (p_a2 == pick && av_a2 == 1) ||
                        (p_a3 == pick && av_a3 == 1) ||
                        (p_b1 == pick && av_b1 == 1) ||
                        (p_b2 == pick && av_b2 == 1)) &&
                    (p_a1 == 0 || av_a1 == 0 || (pick > 0 && pick <= p_a1)) &&
                    (p_a2 == 0 || av_a2 == 0 || (pick > 0 && pick <= p_a2)) &&
                    (p_a3 == 0 || av_a3 == 0 || (pick > 0 && pick <= p_a3)) &&
                    (p_b1 == 0 || av_b1 == 0 || (pick > 0 && pick <= p_b1)) &&
                    (p_b2 == 0 || av_b2 == 0 || (pick > 0 && pick <= p_b2))
                );
        }
    }
}

/// The model ladder: when the live upgrade takes a due model move
/// (`aterm_agent::harness::upgrade_models::model_moves_now`; which model is
/// [`harness_model_priority_model`]'s).
///
/// Every due move lands: at once when `cold`; at once when a newer build restarts the session
/// anyway (the model rides it); else after at most `Warm` readable visits of being due (`clock`).
/// Waiting for a cold cache alone never moves a session in active use. A visit that cannot read
/// the live model (`unknown`: a transcript tail with no answer) decides nothing and leaves the
/// clock, since resetting it would restart the bound on every flicker.
///
/// `NeverPastTheBound`: while the move is due and readable, no visit waits past `Warm`, even
/// when the session stays warm for ever (`Answer`). `Buggy = 1` treats cold as a condition, so
/// a warm session waits every visit and `clock` passes the bound.
///
/// Tier-1: `aterm-agent/tests/conformance_upgrade_models/ladder.rs` drives the real
/// `model_moves_now` over every reachable state and requires `VisitMoves` enabled exactly where
/// it answers a move, with the cold-only rule as the caught negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_model_ladder_model() -> Model {
    aterm_spec::ty_model! {
        HarnessModelLadder {
            const Buggy = 0;
            // MODEL_WARM_MAX_S, in readable visits of being due.
            const Warm = 3;
            var cold = 0;
            var restart = 0;
            var unknown = 0;
            var clock = 0;
            var moved = 0;

            // -- the environment: an answer keeps the cache warm, possibly for ever --------
            action Answer when (moved == 0) {
                cold = 0;
            }
            action GoQuiet when (moved == 0 && cold == 0) {
                cold = 1;
            }
            // A newer Claude Code build is installed: the session restarts onto it anyway.
            action BuildArrives when (moved == 0 && restart == 0) {
                restart = 1;
            }
            action Flicker when (moved == 0 && unknown == 0) {
                unknown = 1;
            }
            action Readable when (moved == 0 && unknown == 1) {
                unknown = 0;
            }

            // -- the harness's visit (readable only) -----------------------
            action VisitMoves when (
                moved == 0 && unknown == 0 &&
                (cold == 1 || (Buggy == 0 && (restart == 1 || clock > Warm - 1)))
            ) {
                moved = 1;
            }
            action VisitWaits when (
                moved == 0 && unknown == 0 && cold == 0 && clock <= Warm &&
                (Buggy == 1 || (restart == 0 && clock <= Warm - 1))
            ) {
                clock = clock + 1;
            }

            invariant NeverPastTheBound: clock <= Warm;
        }
    }
}

/// The model switch (`harness::upgrade_models` + `harness::upgrade_drive`): which model a
/// conversation is moved to, when, and what the harness remembers of it, as one lifecycle for one
/// conversation.
///
/// The rule is `model_due`, "same family first, then the priority list", over the build's offer
/// (`upgrade_models::offered`). On this projection its moves are a list-only rule's: Opus 5 moves
/// to Opus 5.5 by its family's step, whoever chose it; Fable crosses to Opus 5.5 by the list's
/// step, only when nobody chose it; nothing moves down, sideways, or off a model the list does
/// not name. The one exception: a person's launch alias (`PersonFlag = 2`), which the rule keeps
/// verbatim (`model-alias`) before it reads what runs, so nothing is due under it and even an
/// unread visit clears the due clock. A family the list does not name moving by the build's own
/// word (Sonnet to a newer Sonnet) is outside the projection.
///
/// Bound: the Tier-1 file (`aterm-agent`'s `conformance_upgrade_models/switch.rs`) drives these
/// real functions over every reachable state and projects their results onto the model:
///
/// * `Visit` — `upgrade_models::model_read_step`, which `upgrade_drive::model_judge` (the one
///   read the drive's visit, the host's pre-filter and a riding restart all take) wraps in I/O:
///   its early return when no model can run and none is pending, then what the transcript says
///   runs (`live_model_at`), the settle step (`ModelRecord::settle`), the rule (`model_due`) over
///   the settled record, the ride's guard (`ride_bar`) over a notice's model, the cache's
///   coldness (`last_answer_at` against `CACHE_COLD_S`) and the due clock
///   (`ModelRecord::due_clock`), in that order; then `model_to`, with the model ladder
///   (`model_moves_now`) inside it.
/// * `Relaunch`, `RelaunchRunsOther`, `Refused`, `Ride`, `Restart` — the ask recorded before the
///   act (`ModelRecord::asked`), taken back when the act is never sent
///   (`ModelRecord::take_back`), the model the line asks for (`upgrade_models::line_model`: the
///   site's own, else none kept of a `--model` the record marks failed), and what the new process
///   runs (`live_model_at`: its launch flag until it answers).
///
/// Transcribed, not driven (a change there passes the bind): `model_judge`'s and
/// `model_read_kept`'s I/O; the offer itself; `visit_models`' derivation of the carried model
/// (`St::carried_model`), the announced one (`St::promised_model`) and `build_restart`; the drop
/// of a barred ride (`ride_dropped`) and the release of an announced round it leaves with nothing
/// to restart for (`St::release_unridden`); the pre-filter `model_wants_a_look`; `Announce`
/// (`St::for_target`); `GiveUp` (`St::give_up`); the relaunch's ask of `st.model_list`
/// (`relaunch::relaunch`) and its line (`relaunch::with_model`); `Refused`'s `St::unsent`; and
/// which view (managed or native) the session is judged by; the model has one view.
///
/// The projection. Models are `upgrade_models::SEED` plus one it does not name: `live` is `1`
/// `claude-opus-5-5` (rank 1), `2` `claude-fable-5-1` (rank 2), `3` `claude-opus-5` (rank 3),
/// `4` `claude-sonnet-5` (unlisted), `0` unread (`model-unknown`); `cmd` = a `/model` result
/// newer than the last answer (`LiveModel::by_command`). The offer `tgt` is `1` (all seed models),
/// `3` (only `claude-opus-5`: every move would be down) or `0` (none), so the only model ever due,
/// announced or asked for is `claude-opus-5-5`, and `set`/`ap`/`fl`/`due` are its ask, applied,
/// failed and due-clock bits. `sage` = `MODEL_SETTLE_S` has passed since `set_at`, `dover` =
/// `MODEL_WARM_MAX_S` since `due_since`, `cold` = no answer for `CACHE_COLD_S`. `hum` = the
/// remembered `/model` (`ModelRecord::human`) is `claude-fable-5-1`. `flag` = what this harness's
/// relaunches left on the line: `0` nothing (the launch's own flags), `1` its
/// `--model claude-opus-5-5`, `2` no `--model` (dropped once marked failed). `PersonFlag` = the
/// launch carried a person's `--model`: `1` an id (`claude-fable-5-1`), `2` a family alias.
/// `DefaultFable` = the settings' default is Fable; `Build` = a newer build is due throughout.
/// `phase`: `0` not announced, `1` announced (`Phase::Announced`), `2` gave up (`GAVE_UP`: it
/// stops asking, and a late READY still restarts with the notice's model, `ann`). `ann` = the
/// notice carried the model and the round still does. `fresh`/`mto` = the state is the one a visit
/// just left, and that visit's `model_to`; the laws judge that decision against its inputs.
///
/// Laws, one invariant each; `Buggy = 1` arms at least one mutant per law (two for each of the
/// first three and for `SettleRecordsWhatRan`), each firing once from a state where the defect
/// shows, after which nothing moves. The first three are stated over the verdict (`due`), the
/// decision (`mto`) and the ride (`phase > 0 && ann == 1`), each with a `Judge*` and a `Ride*`
/// mutant:
///
/// * `UpTheListOnly` — never down, sideways, or off an unlisted model. `JudgeMovesOffList` reads
///   an unlisted model's missing rank as below everything; `RideMovesOffList` rides from
///   `claude-sonnet-5`.
/// * `PersonsChoiceStaysInFamily` — a person's choice (a launch `--model` not the harness's, a
///   `/model` now or remembered, the settings' default) moves only within its family.
///   `JudgeForgetsTheRememberedChoice` reads the choice only off the launch flag and a fresh
///   `/model`; `RideOverridesAPersonsChoice` moves a `/model` typed after the notice.
/// * `NeverOntoAppliedOrFailed` — never onto a model already applied, or after it failed.
///   `JudgeRetriesAFailedModel` asks for a failed model again every time it is due (a restart
///   loop); `RideCarriesAFailedModel` keeps the ride past the visit that records it failed.
/// * `MovesExactlyWhenTheLadderSays` — a due move is taken exactly when the model ladder says (a
///   cold cache, a restart anyway, or the warm wait over). `LadderWaitsForCold` moves only on a
///   cold cache, so an active session never moves.
/// * `AnnouncedModelRides` — once announced, the model is the one the relaunch asks for, even
///   after READY re-warms the cache. `LadderRetakenAfterReady` re-takes the decision each visit.
/// * `SettleRecordsWhatRan` — an ask that runs is recorded applied; one never applied and still
///   not running after `MODEL_SETTLE_S` is recorded failed and cleared. `SettleNeverFails` (the
///   same model asked again) and `SettleNeverVerifies` (a person moving off it is moved back).
/// * `AppliedAskStands` — an applied ask is never recorded failed. `SettleFailsAnAskThatRan`
///   skips the `applied` test, so a later `/model` turns a verified ask into `model-failed` and
///   the harness's `--model` then reads as a person's.
/// * `AskedOnlyByARelaunch` — an ask on the record is one a relaunch's line carried (`flag`).
///   `RefusedKeepsTheAsk` keeps an ask whose signal was never sent, so it settles failed and the
///   model is barred for good.
/// * `NeverRelaunchedOntoAFailedModel` — a failed model is never what a relaunch starts on.
///   `RelaunchKeepsAFailedFlag`: a line asking no model of its own (the build restart, a restart
///   in place or on exit) keeps the launch's `--model` verbatim (`relaunch::with_model` with
///   `None`), the harness's failed ask included.
///
/// The ride's guard. Unguarded, an announced model rides whatever the visit decided and a
/// gave-up upgrade's late READY relaunches with the notice's `model_list`, asking for
/// `claude-opus-5-5` (a) after it was recorded failed, (b) over a person's `/model
/// claude-fable-5-1` typed after the notice, and (c) off an unlisted model. `ride_bar` (here the
/// `Visit`'s ride branch) applies the three laws to the ride, and none of (a)–(c) is reachable,
/// each still reached through its `Ride*` mutant. Gap (a) had a second road, the line: a relaunch
/// asking no model kept the launch's `--model`, so a failed harness ask rode every later restart.
/// Every relaunch line goes through `upgrade_models::line_model`, which drops such a flag
/// (`flag = 2`) and lets the person's saved default run.
///
/// The Tier-1 file's header lists what this does not cover; the largest omissions are a second
/// family member above the person's (the target is always Opus), a person's own relaunch, the
/// owner's `--now` re-arming a gave-up upgrade, a restart for another reason while an upgrade is
/// announced, a `/model` chosen since the launch that a no-model line carries in place of the
/// launch's `--model` (`hand_model_in`; here such a line keeps the launch flag), and clock skew.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_model_switch_model() -> Model {
    aterm_spec::ty_model! {
        HarnessModelSwitch {
            const Buggy = 0;
            const DefaultFable = 0;
            const PersonFlag = 0;
            const Build = 0;

            var live = 3;
            var cmd = 0;
            var flag = 0;
            var cold = 0;
            var tgt = 1;
            var set = 0;
            var sage = 0;
            var ap = 0;
            var fl = 0;
            var hum = 0;
            var due = 0;
            var dover = 0;
            var phase = 0;
            var ann = 0;
            var fresh = 0;
            var mto = 0;
            var forged = 0;

            // -- the world: every step ends the visit's freshness -----------

            // The conversation answers: the cache is warm, and a `/model`
            // before it is no longer newer than the last answer.
            action Answer when (forged == 0) {
                cold = 0;
                cmd = 0;
                fresh = 0;
                mto = 0;
            }
            action GoCold when (forged == 0 && cold == 0) {
                cold = 1;
                fresh = 0;
                mto = 0;
            }
            // Availability moves the target: Opus 5.5, only Opus 5, none.
            action Retarget when (forged == 0) {
                tgt = if tgt == 1 { 3 } else if tgt == 3 { 0 } else { 1 };
                fresh = 0;
                mto = 0;
            }
            // A person's `/model`: Fable, or a model the list does not name.
            action PersonTypesModel when (forged == 0) {
                live = if live == 2 { 4 } else { 2 };
                cmd = 1;
                fresh = 0;
                mto = 0;
            }
            // The tail holds no answer (a long tool result): unread.
            action LoseSight when (forged == 0 && live > 0) {
                live = 0;
                cmd = 0;
                fresh = 0;
                mto = 0;
            }
            action SettleElapses when (forged == 0 && set == 1 && sage == 0) {
                sage = 1;
                fresh = 0;
                mto = 0;
            }
            action WarmWaitElapses when (forged == 0 && due == 1 && dover == 0) {
                dover = 1;
                fresh = 0;
                mto = 0;
            }

            // -- the harness -------------------------------------------------

            // `model_read_step` + `model_to`, as one visit. The settle step's failed arm fires
            // only on an ask never applied. A notice's model the round carries
            // (`phase > 0 && ann == 1`) is read even on an empty offer, and rides only while
            // the ride's guard (`ride_bar`, over the settled record) says the three move laws
            // hold for it: not applied or failed, a person's Fable not moved across (nor a
            // launch alias at all), and up the list only. Barred, the round forgets it
            // (`ann = 0`; `ride_dropped` clears `St::model_list`).
            action Visit when (forged == 0) {
                ap = if set == 1 && live == 1 { 1 } else { ap };
                fl = if set == 1 && (live <= 0 || live > 1) && sage == 1 && ap == 0 { 1 } else { fl };
                set = if set == 1 && (live <= 0 || live > 1) && sage == 1 && ap == 0 { 0 } else { set };
                sage = if set == 1 && (live <= 0 || live > 1) && sage == 1 && ap == 0 { 0 } else { sage };
                hum = if (tgt > 0 || set == 1 || (phase > 0 && ann == 1)) && cmd == 1 {
                    if live == 2 { 1 } else { 0 }
                } else {
                    hum
                };
                due = if (if tgt == 1 && ap == 0 && fl == 0 && (set == 0 || sage == 0) &&
                    (flag == 1 || PersonFlag <= 1) {
                    if live == 3 {
                        1
                    } else if live == 2 {
                        if (flag == 0 && PersonFlag == 1) || (flag == 1 && set == 0) ||
                            cmd == 1 || hum == 1 || DefaultFable == 1 { 0 } else { 1 }
                    } else {
                        0
                    }
                } else {
                    0
                }) == 1 {
                    1
                } else if tgt > 0 && live <= 0 && (flag == 1 || PersonFlag <= 1) {
                    due
                } else {
                    0
                };
                dover = if (if tgt == 1 && ap == 0 && fl == 0 && (set == 0 || sage == 0) &&
                    (flag == 1 || PersonFlag <= 1) {
                    if live == 3 {
                        1
                    } else if live == 2 {
                        if (flag == 0 && PersonFlag == 1) || (flag == 1 && set == 0) ||
                            cmd == 1 || hum == 1 || DefaultFable == 1 { 0 } else { 1 }
                    } else {
                        0
                    }
                } else {
                    0
                }) == 1 {
                    if due == 1 { dover } else { 0 }
                } else if tgt > 0 && live <= 0 && (flag == 1 || PersonFlag <= 1) {
                    dover
                } else {
                    0
                };
                ann = if phase > 0 && ann == 1 && (if ap == 0 && fl == 0 &&
                    (set == 0 || ((live <= 0 || live > 1) && sage == 0)) &&
                    (flag == 1 || PersonFlag <= 1) &&
                    (live <= 0 || live == 3 || (live == 2 &&
                        (if (flag == 0 && PersonFlag == 1) || (flag == 1 && set == 0) ||
                            cmd == 1 || hum == 1 || DefaultFable == 1 { 0 } else { 1 }) == 1)) {
                    0
                } else {
                    1
                }) == 1 {
                    0
                } else {
                    ann
                };
                mto = if phase == 1 && ann == 1 && ap == 0 && fl == 0 &&
                    (set == 0 || ((live <= 0 || live > 1) && sage == 0)) &&
                    (flag == 1 || PersonFlag <= 1) &&
                    (live <= 0 || live == 3 || (live == 2 &&
                        (if (flag == 0 && PersonFlag == 1) || (flag == 1 && set == 0) ||
                            cmd == 1 || hum == 1 || DefaultFable == 1 { 0 } else { 1 }) == 1)) {
                    1
                } else if (if tgt == 1 && ap == 0 && fl == 0 && (set == 0 || sage == 0) &&
                    (flag == 1 || PersonFlag <= 1) {
                    if live == 3 {
                        1
                    } else if live == 2 {
                        if (flag == 0 && PersonFlag == 1) || (flag == 1 && set == 0) ||
                            cmd == 1 || hum == 1 || DefaultFable == 1 { 0 } else { 1 }
                    } else {
                        0
                    }
                } else {
                    0
                }) == 1 && (cold == 1 || Build == 1 || (due == 1 && dover == 1)) {
                    1
                } else {
                    0
                };
                fresh = 1;
            }
            // The notice, carrying that visit's `model_to`: a restart with nothing to carry is a
            // build's; a build-only notice is retargeted when a model becomes due. A gave-up
            // upgrade is not announced again (`St::for_target` reuses a failed state).
            action Announce when (
                forged == 0 && fresh == 1 &&
                ((phase == 0 && (mto == 1 || Build == 1)) || (phase == 1 && ann == 0 && mto == 1))
            ) {
                phase = 1;
                ann = mto;
                fresh = 0;
                mto = 0;
            }
            // READY, to a standing notice or late after a give-up: the ask recorded, the agent
            // ended, and the relaunch runs what its line asks: the notice's model, else the
            // launch flag it kept (never the harness's own failed ask: `line_model`), else what
            // the transcript last named. There is a restart only while a build is due or this
            // visit moves the model.
            action Relaunch when (forged == 0 && fresh == 1 && phase > 0 && (Build == 1 || mto == 1)) {
                set = if ann == 1 { 1 } else { set };
                sage = if ann == 1 { 0 } else { sage };
                flag = if ann == 1 { 1 } else if flag == 1 && fl == 1 { 2 } else { flag };
                live = if ann == 1 || (flag == 1 && fl == 0) {
                    1
                } else if flag == 0 && PersonFlag == 1 {
                    2
                } else {
                    live
                };
                cmd = if ann == 1 || (flag == 1 && fl == 0) || (flag == 0 && PersonFlag == 1) {
                    0
                } else {
                    cmd
                };
                phase = 0;
                ann = 0;
                fresh = 0;
                mto = 0;
            }
            // The relaunch asked for the model, and Claude Code runs another.
            action RelaunchRunsOther when (
                forged == 0 && fresh == 1 && phase > 0 && ann == 1 && (Build == 1 || mto == 1)
            ) {
                set = 1;
                sage = 0;
                flag = 1;
                live = 3;
                cmd = 0;
                phase = 0;
                ann = 0;
                fresh = 0;
                mto = 0;
            }
            // The signal refused: nothing relaunched, the phase as it was (`St::unsent`), and no
            // ask stands: an ask is written right before its relaunch line is typed and taken
            // back when that line is not (`ModelRecord::take_back`).
            action Refused when (
                forged == 0 && fresh == 1 && phase > 0 && ann == 1 && (Build == 1 || mto == 1)
            ) {
                fresh = 0;
                mto = 0;
            }
            // No READY the restart could act on: the upgrade stops asking. It
            // keeps the notice's `model_list`, and a late READY still restarts.
            action GiveUp when (forged == 0 && phase == 1) {
                phase = 2;
                fresh = 0;
                mto = 0;
            }
            // A restart made for another reason carries the due model.
            action Ride when (forged == 0 && fresh == 1 && phase == 0 && due == 1 && live > 0) {
                set = 1;
                sage = 0;
                flag = 1;
                live = 1;
                cmd = 0;
                fresh = 0;
                mto = 0;
            }
            // With nothing riding (the memory banner's restart in place, an exit's relaunch)
            // the line asks no model: it keeps the launch flag, dropping the harness's own
            // failed ask, as `Relaunch` does with no notice's model.
            action Restart when (forged == 0 && fresh == 1 && phase == 0 && (due == 0 || live <= 0)) {
                flag = if flag == 1 && fl == 1 { 2 } else { flag };
                live = if flag == 1 && fl == 0 { 1 } else if flag == 0 && PersonFlag == 1 { 2 } else { live };
                cmd = if (flag == 1 && fl == 0) || (flag == 0 && PersonFlag == 1) { 0 } else { cmd };
                fresh = 0;
                mto = 0;
            }

            // -- the defects; every healthy branch changes nothing ----------

            action JudgeMovesOffList when (
                forged == 0 && tgt == 1 && live == 4 && cmd == 0 && sage == 0 &&
                ap == 0 && fl == 0 && phase == 0 &&
                ((flag == 0 && PersonFlag == 0) || (flag == 1 && set == 1))
            ) {
                due = if Buggy == 1 { 1 } else { due };
                mto = if Buggy == 1 {
                    if cold == 1 || Build == 1 || (due == 1 && dover == 1) { 1 } else { 0 }
                } else {
                    mto
                };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action JudgeForgetsTheRememberedChoice when (
                forged == 0 && tgt == 1 && live == 2 && cmd == 0 && sage == 0 &&
                ap == 0 && fl == 0 && phase == 0 &&
                ((flag == 0 && PersonFlag == 0) || (flag == 1 && set == 1)) &&
                (hum == 1 || DefaultFable == 1)
            ) {
                due = if Buggy == 1 { 1 } else { due };
                mto = if Buggy == 1 {
                    if cold == 1 || Build == 1 || (due == 1 && dover == 1) { 1 } else { 0 }
                } else {
                    mto
                };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action JudgeRetriesAFailedModel when (
                forged == 0 && tgt == 1 && live == 3 && cmd == 0 && sage == 0 &&
                ap == 0 && fl == 1 && phase == 0
            ) {
                due = if Buggy == 1 { 1 } else { due };
                mto = if Buggy == 1 {
                    if cold == 1 || Build == 1 || (due == 1 && dover == 1) { 1 } else { 0 }
                } else {
                    mto
                };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // The ride unguarded, one law at a time: the visit from a state where the guard
            // bars the ride, every other effect the healthy one's, the ride kept (`mto`, and
            // the `ann` a late READY after a give-up still asks for).
            // (a) The record not read: a model this visit records failed still rides.
            action RideCarriesAFailedModel when (
                forged == 0 && phase > 0 && ann == 1 && live == 3 && cmd == 0 &&
                set == 1 && sage == 1 && ap == 0
            ) {
                fl = if Buggy == 1 { 1 } else { fl };
                set = if Buggy == 1 { 0 } else { set };
                sage = if Buggy == 1 { 0 } else { sage };
                due = if Buggy == 1 { 0 } else { due };
                dover = if Buggy == 1 { 0 } else { dover };
                mto = if Buggy == 1 { if phase == 1 { 1 } else { 0 } } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // (b) A person's choice not read: their `/model claude-fable-5-1`,
            // typed since the notice, moved across to Opus.
            action RideOverridesAPersonsChoice when (
                forged == 0 && phase > 0 && ann == 1 && live == 2 && cmd == 1 &&
                ap == 0 && fl == 0 && (set == 0 || sage == 0)
            ) {
                hum = if Buggy == 1 { 1 } else { hum };
                due = if Buggy == 1 { 0 } else { due };
                dover = if Buggy == 1 { 0 } else { dover };
                mto = if Buggy == 1 { if phase == 1 { 1 } else { 0 } } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // (c) The list not read: a model it does not name moved up to Opus.
            action RideMovesOffList when (
                forged == 0 && phase > 0 && ann == 1 && live == 4 && cmd == 0 &&
                ap == 0 && fl == 0 && (set == 0 || sage == 0)
            ) {
                due = if Buggy == 1 { 0 } else { due };
                dover = if Buggy == 1 { 0 } else { dover };
                mto = if Buggy == 1 { if phase == 1 { 1 } else { 0 } } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action LadderWaitsForCold when (
                forged == 0 && tgt == 1 && live == 3 && cmd == 0 && sage == 0 &&
                ap == 0 && fl == 0 && phase == 0 && cold == 0 &&
                (Build == 1 || (due == 1 && dover == 1))
            ) {
                due = if Buggy == 1 { 1 } else { due };
                mto = if Buggy == 1 { 0 } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // Re-taken where the fresh decision would not carry the model: the READY answer
            // warmed the cache (and nothing else says move), or the target moved away.
            action LadderRetakenAfterReady when (
                forged == 0 && live == 3 && cmd == 0 && sage == 0 && phase == 1 && ann == 1 && (
                    (tgt == 1 && ap == 0 && fl == 0 && cold == 0 && Build == 0 &&
                        (due == 0 || dover == 0)) ||
                    (tgt == 3 && due == 0)
                )
            ) {
                due = if Buggy == 1 && tgt == 1 { 1 } else { due };
                mto = if Buggy == 1 { 0 } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action SettleNeverFails when (
                forged == 0 && set == 1 && sage == 1 && live > 1 && ap == 0 && tgt == 3 &&
                cmd == 0 && due == 0 && phase == 0
            ) {
                mto = if Buggy == 1 { 0 } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action SettleNeverVerifies when (
                forged == 0 && set == 1 && live == 1 && ap == 0 && tgt == 3 &&
                cmd == 0 && due == 0 && phase == 0
            ) {
                mto = if Buggy == 1 { 0 } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // A refusal that keeps the ask written before a signal never sent, from a process
            // no relaunch of this harness asked anything of.
            action RefusedKeepsTheAsk when (
                forged == 0 && fresh == 1 && phase > 0 && ann == 1 && (Build == 1 || mto == 1) &&
                flag == 0
            ) {
                set = if Buggy == 1 { 1 } else { set };
                sage = if Buggy == 1 { 0 } else { sage };
                fresh = if Buggy == 1 { 0 } else { fresh };
                mto = if Buggy == 1 { 0 } else { mto };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // The line unguarded: a relaunch asking no model of its own (the build restart, a
            // restart with nothing riding) keeps the launch's `--model` verbatim, the
            // harness's own failed ask included, and runs it again.
            action RelaunchKeepsAFailedFlag when (
                forged == 0 && fresh == 1 && flag == 1 && fl == 1 && (
                    (phase == 0 && (due == 0 || live <= 0)) ||
                    (phase > 0 && ann == 0 && (Build == 1 || mto == 1))
                )
            ) {
                live = if Buggy == 1 { 1 } else { live };
                cmd = if Buggy == 1 { 0 } else { cmd };
                phase = if Buggy == 1 { 0 } else { phase };
                fresh = if Buggy == 1 { 0 } else { fresh };
                mto = if Buggy == 1 { 0 } else { mto };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // The failed arm without its `applied` test: an ask that ran, which a person has
            // since moved off, recorded failed and cleared.
            action SettleFailsAnAskThatRan when (
                forged == 0 && set == 1 && sage == 1 && ap == 1 && fl == 0 && live > 1 &&
                due == 0 && phase == 0
            ) {
                set = if Buggy == 1 { 0 } else { set };
                sage = if Buggy == 1 { 0 } else { sage };
                fl = if Buggy == 1 { 1 } else { fl };
                mto = if Buggy == 1 { 0 } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }

            // A visit's verdict is due exactly when it leaves the clock running on a model it
            // could read (`fresh == 1 && due == 1 && live > 0`; `model-unknown` leaves the clock
            // as it was). Its decision is `mto`; while an announced round's model rides
            // (`phase == 1 && ann == 1`) that is the ride. The ride (`phase > 0 && ann == 1`)
            // is held to the first three laws over what the visit could read, the target fixed
            // by the notice and the offer not re-read.

            // The move is onto the target, a listed model ranked above the live one, and the
            // live one is listed; the ride's from Opus 5 or Fable 5.1.
            invariant UpTheListOnly:
                fresh == 0 ||
                (((tgt == 1 && live > 1 && live <= 3) ||
                    ((due == 0 || live <= 0) && (mto == 0 || (phase == 1 && ann == 1)))) &&
                 (phase <= 0 || ann == 0 || live <= 0 || (live > 1 && live <= 3)));
            // The target's family is Opus (`1` and `3`).
            invariant PersonsChoiceStaysInFamily:
                fresh == 0 || live == 1 || live == 3 ||
                (if (flag == 0 && PersonFlag > 0) || (flag == 1 && set == 0) || cmd == 1 ||
                    (live == 2 && (hum == 1 || DefaultFable == 1)) { 0 } else { 1 }) == 1 ||
                ((due == 0 || live <= 0) && (mto == 0 || (phase == 1 && ann == 1)) &&
                 (phase <= 0 || ann == 0 || live <= 0));
            invariant NeverOntoAppliedOrFailed:
                fresh == 0 || (ap == 0 && fl == 0) ||
                ((due == 0 || live <= 0) && mto == 0 && (phase <= 0 || ann == 0));
            // Outside an announcement, `model_to` is the due move exactly when the ladder
            // takes it.
            invariant MovesExactlyWhenTheLadderSays:
                fresh == 0 || (phase == 1 && ann == 1) ||
                mto == (if due == 1 && live > 0 && (cold == 1 || Build == 1 || dover == 1) { 1 } else { 0 });
            invariant AnnouncedModelRides:
                fresh == 0 || phase <= 0 || phase > 1 || ann == 0 || mto == 1;
            invariant SettleRecordsWhatRan:
                fresh == 0 || set == 0 || (live == 1 && ap == 1) ||
                ((live <= 0 || live > 1) && (ap == 1 || sage == 0));
            invariant AppliedAskStands:
                ap == 0 || set == 1;
            // The harness's `--model` is on the process (dropped from the line only once that
            // ask is recorded failed and cleared).
            invariant AskedOnlyByARelaunch:
                set == 0 || flag == 1;
            // `live` reads the failed model only off a line naming it, so this is the line
            // never carrying it.
            invariant NeverRelaunchedOntoAFailedModel:
                fl == 0 || live <= 0 || live > 1;
        }
    }
}
