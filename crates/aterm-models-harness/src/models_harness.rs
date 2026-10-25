// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded models for the harness core (`docs/DESIGN-aterm-wrapper-2026-09-17.md` §11): the bounded child runner's worker lifecycle (`align::capture_bounded`), the Claude Code upgrade ladder and its notice, drain, never-strands, work-end and limit-queue laws, the login wall, the usage tap, and the model priority, ladder and switch, all in `aterm_agent::harness`.

use aterm_spec::derive::*;

/// A bounded harness capture may time out and reap its direct child while an escaped descendant still holds stdout open; the capture returns only after the stdout reader has finished or been cancelled and joined.
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

/// The live upgrade's ladder: prefer a natural pause, and the longer a session has been behind the less it waits.
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

            action Advance when (behind == 1 && rung <= LandRung - 1) {
                rung = rung + 1;
            }
            action TurnStarts when (work == 0) {
                work = 1;
                quiet = 0;
                seen = 0;
            }
            action TurnEnds when (work == 1) {
                work = 0;
            }
            action Repaint when (behind == 1) {
                quiet = 0;
            }
            action Still when (behind == 1 && work == 0 && quiet == 0) {
                quiet = 1;
            }
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
            action SeqSettle when (
                Buggy == 1 && slipped == 0 &&
                behind == 1 && work == 0 && draft == 0 && dialog == 0 &&
                (keys == 0 || (rung > 1 && narrows == 1 && keys <= 1)) &&
                rung > 0 && rung <= LandRung - 1 && seen == 2 && quiet == 0 && missed == 0
            ) {
                slipped = 1;
                missed = 1;
            }
            action AttendedWithoutLadder when (
                Buggy == 1 && slipped == 0 &&
                narrows == 1 && behind == 1 && rung > 1 && keys == 1 &&
                work == 0 && draft == 0 && dialog == 0
            ) {
                slipped = 1;
                narrows = 0;
            }
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

/// The upgrade lands: the ladder's law as a property of whole behaviours (activity delays, it never disables): `[]<>(moved \/ a-floor-stands)`.
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

/// A READY answer is usable only by the process and tab that received the notice, with a unique live owner of that conversation and a complete session-file scan.
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
            action Resume when (phase == 1 && tab == 1 && pid <= 1) {
                pid = 2;
                start = 2;
            }
            action OwnerExits when (
                phase == 1 && gone == 0 && owner_pid == 1 && owner_start == 1
            ) {
                gone = 1;
                pid = if (pid == owner_pid && start == owner_start) { 0 } else { pid };
                start = if (pid == owner_pid && start == owner_start) { 0 } else { start };
            }
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

/// The upgrade drain's bounds (`aterm_agent::harness::upgrade`, `DRAIN_S`, `HOLD_S`, `REASK_S`, `MAX_ASKS`).
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
            action StatusLags when (ended == 0 && gaveup == 0 && lag == 0) { lag = 1; }
            action StatusIdle when (ended == 0 && gaveup == 0 && lag == 1) { lag = 0; }

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

/// The live upgrade never strands the agent it asked (`aterm_agent::harness::upgrade`, `next_step`, `gate_announce`, `clock_held`, `gate_release`, `transcript_has_ready`, `directed_since_ready`, and the driver's record transitions).
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

            action LimitHits when (limited == 0 && (phase <= 2 || phase == 4)) {
                limited = 1;
            }
            action LimitResets when (limited == 1) {
                limited = 0;
                holding = if unread == 1 { 1 } else { holding };
                unread = 0;
                window = if (phase == 1 && Buggy == 0 && NoF1 == 0) { 0 } else { window };
            }
            action AgentReady when (limited == 0 && holding == 1 && ready == 0 && live == 1) {
                ready = 1;
                directed = if ((StaleDirection == 1 || Buggy == 1) && told == 1) { 1 } else if ((StaleDirection == 1 || Buggy == 1)) { directed } else { 0 };
                told = 0;
            }
            action AgentGoesOn when (limited == 0 && holding == 1 && ready == 0) {
                holding = 0;
                directed = if told == 1 { 1 } else { directed };
                told = 0;
            }
            action Direct when (limited == 0 && holding == 1 && told == 0 && directed == 0) {
                told = 1;
                ready = if (ReadyOverDirection == 1 || Buggy == 1) { ready } else { 0 };
            }
            action Elapse when (
                phase == 1 && window == 0 && (limited == 0 || Buggy == 1 || NoF1 == 1)
            ) {
                window = 1;
            }
            action BreakBegins when (brk == 0) {
                brk = 1;
            }
            action BreakEnds when (brk == 1) {
                brk = 0;
            }
            action Rests when ((phase == 2 || phase == 4) && rest == 0) {
                rest = 1;
            }
            action BuildFails when (fails == 0) {
                fails = 1;
            }
            action BuildStarts when (fails == 1) {
                fails = 0;
            }

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
            action GiveUpOutlived when (
                ready == 1 && live == 1 && limited == 0 && phase == 1 &&
                (fails == 0 || HeldAsked == 1) &&
                window == 1 && asks == MaxAsks
            ) {
                phase = 2;
                owed = if NoOwe == 1 { owed } else { 1 };
                rest = 0;
            }
            action Void when (
                ready == 1 && live == 1 && limited == 0 && (fails == 0 || HeldAsked == 1) &&
                ((phase == 1 && window == 1) ||
                 (phase == 2 && Buggy == 0 && NoF2 == 0 && (brk == 0 || IdleOnlyRelease == 0)))
            ) {
                ready = 0;
                live = 0;
                owed = if NoOwe == 1 { owed } else { 1 };
                window = if (phase == 1 && Buggy == 0 && NoOwe == 0) { 0 } else { window };
                rest = 0;
            }
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
            action Abandon when (phase == 1 || (phase == 2 && ready == 1 && live == 1)) {
                phase = 4;
                ready = if KeepReady == 1 { ready } else { 0 };
                live = if KeepReady == 1 { live } else { 0 };
                owed = if NoOwe == 1 { owed } else { 1 };
                rest = 0;
            }
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

/// The end of the agent's own work is the upgrade's point (aterm-agent `harness/upgrade.rs`, `next_step` over `Facts::work_ended`; `harness/upgrade_drive.rs`, `St::work_seen`, `St::time_work` and the visit's same-visit re-arm).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_work_end_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeWorkEnd {
            const Buggy = 0;
            const Nag = 0;
            const Hold = 0;
            const Cap = 3;
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

            action WorkRuns when (screen == 0) {
                screen = 1;
                worked = 1;
                looked = 0;
                owns = 0;
            }
            action WorkEnds when (screen == 1) {
                screen = 0;
                looked = 0;
                owns = 0;
            }
            action StatusLags when (screen == 0 && lag == 0) {
                lag = 1;
                looked = 0;
                owns = 0;
            }
            action WindowPasses when (phase == 1 && late == 0) {
                late = 1;
                looked = 0;
                owns = 0;
            }
            action GivesUp when (phase == 1 && screen == 1) {
                phase = 2;
            }
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
            action HostHolds when (
                screen == 0 && looked == 0 && worked == 1 && lag == 1 &&
                (phase == 2 || late == 0 || Hold == 1 || Buggy == 1)
            ) {
                stuck = if (phase == 1 && late == 1) { 1 } else { stuck };
                looked = 1;
                owns = 1;
            }
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
            action HostGivesUp when (
                phase == 1 && screen == 0 && looked == 0 && late == 1 && asks > Cap - 1 &&
                (worked == 0 || (lag == 1 && Hold == 0))
            ) {
                phase = 2;
                late = 0;
                looked = 1;
                owns = 0;
            }
            action HostWaits when (
                screen == 0 && looked == 0 && (worked == 0 || Buggy == 1) &&
                (phase == 2 || late == 0)
            ) {
                looked = 1;
                owns = 0;
            }
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

/// The login wall (`aterm_agent::harness::upgrade`: `gate_announce`, `next_step`, `announce_asks`, `transcript_login`, `clock_held` and the driver's `login_facts`; `aterm_agent::supervise::policy::turn_end`: `decide_turn_end`'s auth arm, its login hold and `login_wall_waits`; both over `aterm_phase`'s wall reader).
///
/// A roster row's lost login (`rostered`) is signed in by aterm itself (`Renew`, the sign-in ladder; the owner, 2026-10-24: sign-in is autonomous, and no person is asked): the tab types no `/login`, whose browser sign-in only a person could finish (`NoRosterRowSignedInByAPerson`), and is relaunched onto the login the renewal made. `RenewAtLogin` is the roster learning, and the ladder renewing, the row of a login whose `/login` aterm typed before the row was known (the dialog it opened still up), which the relaunch then follows; its guard leaves out the states a mutant already broke a law in (`relooped`, `overrode`), which keeps Buggy=4's space inside the interpreter's budget.
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
            const LoginOnRoster = 0;
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
            var asked = 0;

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
            action BackElsewhere when (
                track == 1 && login == 0 && elsewhere == 0 && back == 0 && renewed == 0
            ) {
                login = 1;
                elsewhere = 2;
            }
            action Inherit when (phase == 0 && asks == 0 && inherited == 0 && stood == 1) {
                phase = 2;
                asks = MaxAsks;
                unread = 1;
                window = 1;
                inherited = 1;
            }

            action Enrol when (
                rostered == 0 && login == 1 && wall == 0 && stood == 0 && track == 0 && back == 0
            ) {
                rostered = 1;
                known = 1;
            }
            action RenewAtLogin when (
                rostered == 0 && track == 1 && wall == 1 && picker == 1 && back == 0 &&
                elsewhere == 0 && left == 0 && done == 0 && relooped == 0 && overrode == 0
            ) {
                rostered = 1;
                known = 1;
                renewed = 1;
            }
            action Profiled when (known == 0) {
                known = 1;
            }
            action Renew when (rostered == 1 && renewed == 0) {
                renewed = 1;
                checked = if ((Buggy == 3 || CheckIdle == 1) && login == 1) { 1 } else { checked };
                login = if (Buggy == 3 || CheckIdle == 1) { 0 } else { login };
            }

            action TypeLogin when (
                ((wall == 1 && track == 0 &&
                    (renewed == 0 || Buggy == 3 || NoFollow == 1) &&
                    (Buggy <= 1 || Buggy > 2) && NoSee == 0 &&
                    (rostered == 0 || Buggy == 3 || LoginOnRoster == 1) &&
                    (done == 0 ||
                        ((Buggy <= 2 || Buggy > 3) && NoDoneWall == 0 && GraceHolds == 0)) &&
                    (left == 0 || Buggy == 4 || ReloginOverLeft == 1) &&
                    (elsewhere == 0 || Buggy == 6 || NoLatch == 1)) ||
                    (track == 1 && wall == 0 && stood == 1 && back == 0 && renewed == 0 &&
                        (Buggy == 4 || Relogin == 1)))
            ) {
                relooped = if (track == 1) { 1 } else { relooped };
                asked = if (rostered == 1) { 1 } else { asked };
                overrode = if (left == 1) { 1 } else { overrode };
                doubled = if (elsewhere == 0) { doubled } else { 1 };
                stranded = if (renewed == 1) { 1 } else { stranded };
                track = 1;
                told = 1;
                picker = 1;
            }
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
            action TellClose when (
                back == 0 && track == 1 && wall == 1 &&
                ((elsewhere == 2 && SignInOverBack == 0) ||
                    (renewed == 1 && SignInOverRenewed == 0))
            ) {
                told = 1;
            }
            action Rest when (
                wall == 1 && track == 0 && told == 0 && renewed == 0 && done == 1 &&
                (Buggy == 3 || NoDoneWall == 1)
            ) {
                silent = 1;
            }
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
            action Follow when (
                renewed == 1 && wall == 0 && stood == 0 && (Buggy <= 2 || Buggy > 3) && NoFollow == 0
            ) {
                login = 1;
                renewed = 0;
                elsewhere = 0;
            }
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
            invariant NoRosterRowSignedInByAPerson: asked == 0;
        }
    }
}

/// The usage tap (aterm-agent's `harness::tap`; the accounts design v2 §3.1, §3.2): one tab's Claude Code across aterm's relaunches, the status-line tap aterm may put on the line, the readings it writes, and the host's read of them at its decided turn ends. The owner's own statusLine (`userline`) no longer keeps the tap off (owner ruling R-12): the line carries it (`tap --then`), so the owner's row still shows; `NoChainLook` is a gate that taps without looking at the settings chain, putting the plain tap over that row (`displaced`).
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
            const NoEpoch = 0;
            const MaxPid = 2;
            const MaxApi = 2;
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
            var refill = 0;
            var epoch = 0;
            var served = 0;
            var strike = 0;
            var silent = 0;
            var said = 0;
            var stale = 0;
            var prior = 0;
            var displaced = 0;
            var unattrib = 0;

            action Switch when (MaxPid > pid) {
                armed = 1 - armed;
            }
            action OwnLine when (MaxPid > pid) {
                userline = 1 - userline;
            }
            action Credential when (pid > 0) {
                cred = 1 - cred;
            }

            action Launch when (MaxPid > pid) {
                pid = pid + 1;
                tapped = if (armed == 1 && cred == 1) { 1 } else { 0 };
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

            action Reply when (pid > 0 && MaxApi > api) {
                api = api + 1;
                served = 1;
                file = tapped;
            }
            action RefillReply when (pid > 0 && MaxApi > api && figs > 0 && refill == 0) {
                api = api + 1;
                served = 1;
                figs = 0;
                refill = 1;
                file = tapped;
            }
            action BgUsage when (pid > 0 && 1 > figs) {
                figs = figs + 1;
                file = tapped;
            }
            action IdleReport when (pid > 0) {
                file = tapped;
            }

            action Read when (pid > 0 && file == 1) {
                prior = if (
                    (pid > lpid || lpid > pid || api > lapi || figs > lfigs || lfigs > figs ||
                        Buggy == 1 || NoFresh == 1) &&
                    (cred == 1 || Buggy == 1 || NoAttrib == 1) &&
                    pid == lpid && api > lapi && lfigs > figs &&
                    (Buggy == 1 || NoEpoch == 1) && noted > figs
                ) { 1 } else { prior };
                epoch = if (
                    (pid > lpid || lpid > pid || api > lapi || figs > lfigs || lfigs > figs ||
                        Buggy == 1 || NoFresh == 1) &&
                    (cred == 1 || Buggy == 1 || NoAttrib == 1) &&
                    pid == lpid && api > lapi && lfigs > figs &&
                    Buggy == 0 && NoEpoch == 0
                ) { 1 } else { epoch };
                noted = if (
                    (pid > lpid || lpid > pid || api > lapi || figs > lfigs || lfigs > figs ||
                        Buggy == 1 || NoFresh == 1) &&
                    (cred == 1 || Buggy == 1 || NoAttrib == 1) &&
                    pid == lpid && api > lapi && lfigs > figs &&
                    Buggy == 0 && NoEpoch == 0
                ) {
                    figs
                } else if (
                    (pid > lpid || lpid > pid || api > lapi || figs > lfigs || lfigs > figs ||
                        Buggy == 1 || NoFresh == 1) &&
                    (cred == 1 || Buggy == 1 || NoAttrib == 1) &&
                    (pid == lpid || epoch == 0 || Buggy == 1 || NoEpoch == 1) &&
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

            invariant NeverNotesStale: stale == 0 && prior == 0;
            invariant NeverArmsOverTheOwnersLine: displaced == 0;
            invariant NoTapOnlyAfterServe: silent == 0 || served == 1;
            invariant NothingNotedUnattributed: unattrib == 0;
            invariant NoMarkOutlivesItsProcess: said == 0 || silent == 1;
        }
    }
}

/// The notice queued behind a usage limit (`aterm_agent::harness::upgrade`: `notice_scan`, `queued_until`, `queued_copies`, `Scan::rests_until`, `queue_rest`, `transcript_limit_until`, `next_step`, `requested_step`, `announce_asks`, `clock_held`; the driver's `queue_facts`).
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

            action LimitHits when (limited == 0 && untaken == 0 && phase <= 1) {
                limited = 1;
                shown = 1;
                held = 1;
                window = 0;
            }
            action LimitEnds when (limited == 1) {
                limited = 0;
            }
            action BoundPasses when (held == 1) {
                held = 0;
            }
            action Clear when (shown == 1) {
                shown = 0;
            }
            action GoesOn when (limited == 0 && untaken > 0) {
                untaken = 0;
                took = took + 1;
                shown = 0;
                held = 0;
                rested = 0;
                hurried = 0;
                window = if Buggy == 1 { window } else { 0 };
            }
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
            action Rests when (phase == 1 && untaken > RetypeMax && held == 0 && rested == 0) {
                rested = if (Flat == 0 && Buggy == 0 && untaken > RetypeMax + Daily) { 2 } else { 1 };
            }
            action Elapse when (
                phase == 1 && window == 0 && shown == 0 &&
                (Buggy == 1 ||
                    (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                        (untaken == 0 || NoBound == 0)))
            ) {
                window = 1;
            }
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

/// The model priority list (`harness::upgrade_models`): a writer/reader pair, the shape of `NativeUpdateFailedMarkSuppression`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_model_priority_model() -> Model {
    aterm_spec::ty_model! {
        HarnessModelPriority {
            const Buggy = 0;

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

            invariant AutoInsertIsNewerThanTheFamilysBest: p_a0 == 0;
            invariant NeverExtendedDownward: p_ah == 0;
            invariant AutoInsertIsSameFamily: p_c1 == 0;
            invariant AutoInsertIsKnownToTheBuild:
                (p_a3 == 0 || k_a3 == 1) && (p_b2 == 0 || k_b2 == 1);
            invariant AutoInsertSitsDirectlyAboveTheFamilysBest:
                (p_a3 == 0 || p_a3 + 1 == (if p_a1 <= p_a2 { p_a1 } else { p_a2 })) &&
                (p_b2 == 0 || p_b2 + 1 == p_b1);
            invariant HumanOrderIsNeverRewritten:
                if hord == 0 {
                    p_a2 + 1 <= p_b1 && p_b1 + 1 <= p_a1
                } else if hord == 1 {
                    p_a1 + 1 <= p_a2 && p_a2 + 1 <= p_b1
                } else {
                    p_b1 + 1 <= p_a1 && p_a1 + 1 <= p_a2
                };
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

/// The model ladder: when the live upgrade takes a due model move (`aterm_agent::harness::upgrade_models::model_moves_now`; which model is [`harness_model_priority_model`]'s).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_model_ladder_model() -> Model {
    aterm_spec::ty_model! {
        HarnessModelLadder {
            const Buggy = 0;
            const Warm = 3;
            var cold = 0;
            var restart = 0;
            var unknown = 0;
            var clock = 0;
            var moved = 0;

            action Answer when (moved == 0) {
                cold = 0;
            }
            action GoQuiet when (moved == 0 && cold == 0) {
                cold = 1;
            }
            action BuildArrives when (moved == 0 && restart == 0) {
                restart = 1;
            }
            action Flicker when (moved == 0 && unknown == 0) {
                unknown = 1;
            }
            action Readable when (moved == 0 && unknown == 1) {
                unknown = 0;
            }

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

/// The model switch (`harness::upgrade_models` + `harness::upgrade_drive`): which model a conversation is moved to, when, and what the harness remembers of it, as one lifecycle for one conversation.
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
            action Retarget when (forged == 0) {
                tgt = if tgt == 1 { 3 } else if tgt == 3 { 0 } else { 1 };
                fresh = 0;
                mto = 0;
            }
            action PersonTypesModel when (forged == 0) {
                live = if live == 2 { 4 } else { 2 };
                cmd = 1;
                fresh = 0;
                mto = 0;
            }
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
            action Announce when (
                forged == 0 && fresh == 1 &&
                ((phase == 0 && (mto == 1 || Build == 1)) || (phase == 1 && ann == 0 && mto == 1))
            ) {
                phase = 1;
                ann = mto;
                fresh = 0;
                mto = 0;
            }
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
            action Refused when (
                forged == 0 && fresh == 1 && phase > 0 && ann == 1 && (Build == 1 || mto == 1)
            ) {
                fresh = 0;
                mto = 0;
            }
            action GiveUp when (forged == 0 && phase == 1) {
                phase = 2;
                fresh = 0;
                mto = 0;
            }
            action Ride when (forged == 0 && fresh == 1 && phase == 0 && due == 1 && live > 0) {
                set = 1;
                sage = 0;
                flag = 1;
                live = 1;
                cmd = 0;
                fresh = 0;
                mto = 0;
            }
            action Restart when (forged == 0 && fresh == 1 && phase == 0 && (due == 0 || live <= 0)) {
                flag = if flag == 1 && fl == 1 { 2 } else { flag };
                live = if flag == 1 && fl == 0 { 1 } else if flag == 0 && PersonFlag == 1 { 2 } else { live };
                cmd = if (flag == 1 && fl == 0) || (flag == 0 && PersonFlag == 1) { 0 } else { cmd };
                fresh = 0;
                mto = 0;
            }

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

            invariant UpTheListOnly:
                fresh == 0 ||
                (((tgt == 1 && live > 1 && live <= 3) ||
                    ((due == 0 || live <= 0) && (mto == 0 || (phase == 1 && ann == 1)))) &&
                 (phase <= 0 || ann == 0 || live <= 0 || (live > 1 && live <= 3)));
            invariant PersonsChoiceStaysInFamily:
                fresh == 0 || live == 1 || live == 3 ||
                (if (flag == 0 && PersonFlag > 0) || (flag == 1 && set == 0) || cmd == 1 ||
                    (live == 2 && (hum == 1 || DefaultFable == 1)) { 0 } else { 1 }) == 1 ||
                ((due == 0 || live <= 0) && (mto == 0 || (phase == 1 && ann == 1)) &&
                 (phase <= 0 || ann == 0 || live <= 0));
            invariant NeverOntoAppliedOrFailed:
                fresh == 0 || (ap == 0 && fl == 0) ||
                ((due == 0 || live <= 0) && mto == 0 && (phase <= 0 || ann == 0));
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
            invariant AskedOnlyByARelaunch:
                set == 0 || flag == 1;
            invariant NeverRelaunchedOntoAFailedModel:
                fl == 0 || live <= 0 || live > 1;
        }
    }
}
