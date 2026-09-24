// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded models for the aterm wrapper's harness core
//! (`docs/DESIGN-aterm-wrapper-2026-09-17.md` §11 item 7).
//!
//! Two scalar projections of machines that ship in `aterm_agent::harness`:
//! the limit-recovery engine (`harness::limits`) and the bounded child
//! runner's worker lifecycle (`harness::align::capture_bounded`). As
//! everywhere in this crate the model is hand-written Rust DESCRIBING that
//! code, not extracted from it: Tier 0 here says the description holds over
//! its whole bounded space, and only the Tier-1 binds —
//! `aterm-agent/tests/conformance_harness.rs` for the engine, `align`'s own
//! tests for the runner — make either a statement about the program that
//! compiled.
//!
//! The ledger ring (`HarnessLedgerRing`, §11 item 5) and the grid spine's turn
//! machine (`HarnessTurnObservation`, §4.2/§5.8.1) were modelled here too.
//! Their subsystems, `harness::ring` and `harness::observe`, were deleted with
//! the second harness stack on 2026-09-23, and a model of code that no longer
//! exists proves nothing about anything; they went in the same change.

use super::*;

/// The limit-recovery engine's per-generation ladder (design §11 item 7).
///
/// `class` is the failure class in the index order of `Class::ALL`:
/// `0` transient-capacity, `1` network-offline, `2` session-5h, `3` weekly-7d,
/// `4` model-bucket-limit, `5` spend-billing, `6` auth, `7` unknown. `act` is
/// the last action the engine decided, in a coding of the closed vocabulary:
/// `0` none, `1` let-vendor-retry, `2` wait, `3` retry, `4` switch-model or
/// switch-account, `5` relogin, `6` escalate. `phase` is `0` no live
/// classification (the host holds no state, or the generation settled), `1`
/// classified with nothing in flight, `2` one automatic action awaiting its
/// verdict, `4` escalated — the human's, terminal for the generation.
///
/// `inflight` counts automatic actions awaiting a verdict, `spent` the
/// switches inside one budget window (model and account share it), `dwell`
/// that a switch just landed, `gen` that the generation changed under the
/// state. `stale`, `auto` and `silent` are witnesses the shipping engine never
/// sets: an action that ran for a request carrying a generation the state no
/// longer has, an escalated generation resumed with no human in it, and a
/// generation whose class moved while its ladder cursor stayed behind, so the
/// new row was walked past its end and nobody was ever told.
///
/// The guards are the shipped table's, not a summary of it: `let-vendor-retry`
/// only where the row has it, `wait`/`retry` only for the four classes whose
/// rows carry them, `retry` only after a `wait` was armed (`act == 2`),
/// `switch-*` only for transient / session-5h / weekly-7d and only with budget
/// left and the dwell elapsed, `relogin` only for auth, and `escalate`
/// everywhere (every row must contain it).
///
/// The class and the cursor are ONE value, and two actions state it: a hook
/// value the engine cannot place forces `unknown` and the `unknown` ladder is
/// entered at its beginning (`UnplaceableHookValue`), and a generation
/// reclassified as a different class adopts it and starts that row over
/// (`ReclassifiedAsSpendBilling`). Both are written with nothing in flight,
/// which is where the engine MAKES decisions; a class that moves while an
/// action is out decides nothing at all, because every step is refused until
/// the verdict lands, and that state is deliberately outside this machine.
///
/// `Buggy = 1` arms one defect per design claim: a second automatic action
/// started while one is in flight (`SecondActionWhileInFlight`, the defect the
/// task names), a `switch-*` in a class whose row may never carry one, a
/// `retry` for `unknown`, an action outside the pinned rows of `spend-billing`
/// and `auth`, an escalated generation resumed automatically, a request bound
/// to a stale generation acted on instead of refused, a forced `unknown` whose
/// cursor stays where the old row left it, and a reclassification the old
/// row's decision outlives. `broke` saturates the outside-the-row witnesses so
/// the `Buggy = 1` graph stays finite.
///
/// The second in-flight action is what breaks `BudgetHeld` as well as
/// `OneInFlight`, and that is the finding rather than an accident: two actions
/// tested against the budget before either landed spend two slots out of a
/// bound that had room for one.
///
/// Deliberately absent: the T1/T2 timers, the human pause, the two-source
/// rule (`Classification::unpaired`, spelled `display_only` until the rule
/// was narrowed on 2026-09-19), the relogin count and the retry-cancelling
/// vendor resume. Those are timing and evidence rules the engine's own tests
/// cover; this machine states the §11 laws about WHICH action may run and
/// how many at once.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_failure_recovery_model() -> Model {
    crate::ty_model! {
        HarnessFailureRecovery {
            const Buggy = 0;
            const Budget = 2;

            var class = 0;
            var phase = 0;
            var inflight = 0;
            var act = 0;
            var spent = 0;
            var dwell = 0;
            var gen = 0;
            var stale = 0;
            var auto = 0;
            var silent = 0;
            var broke = 0;

            // -- classification: the host builds a state for one generation --

            action ClassifyTransient when (phase == 0) {
                class = 0;
                phase = 1;
                act = 0;
            }
            action ClassifyNetworkOffline when (phase == 0) {
                class = 1;
                phase = 1;
                act = 0;
            }
            action ClassifySession5h when (phase == 0) {
                class = 2;
                phase = 1;
                act = 0;
            }
            action ClassifyWeekly7d when (phase == 0) {
                class = 3;
                phase = 1;
                act = 0;
            }
            action ClassifyModelBucket when (phase == 0) {
                class = 4;
                phase = 1;
                act = 0;
            }
            action ClassifySpendBilling when (phase == 0) {
                class = 5;
                phase = 1;
                act = 0;
            }
            action ClassifyAuth when (phase == 0) {
                class = 6;
                phase = 1;
                act = 0;
            }
            action ClassifyUnknown when (phase == 0) {
                class = 7;
                phase = 1;
                act = 0;
            }

            // -- the L0/L1 candidates: decided and done, no verdict awaited --

            action ObserveOnly when (
                phase == 1 && inflight == 0 &&
                (class == 0 || class == 1 || class == 4)
            ) {
                act = 1;
            }

            action ArmWait when (
                phase == 1 && inflight == 0 && class <= 3
            ) {
                act = 2;
            }

            action Escalate when (phase == 1 && inflight == 0) {
                act = 6;
                phase = 4;
            }

            // -- the L3/L4 candidates: one at a time, each awaiting a verdict --

            action StartSwitch when (
                phase == 1 && inflight == 0 && dwell == 0 &&
                spent <= Budget - 1 &&
                (class == 0 || class == 2 || class == 3)
            ) {
                inflight = inflight + 1;
                act = 4;
                phase = 2;
            }

            action StartRetry when (
                phase == 1 && inflight == 0 && class <= 2 && act == 2
            ) {
                inflight = inflight + 1;
                act = 3;
                phase = 2;
            }

            action StartRelogin when (
                phase == 1 && inflight == 0 && class == 6
            ) {
                inflight = inflight + 1;
                act = 5;
                phase = 2;
            }

            // An executed switch starts the dwell and spends one budget slot.
            // The generation is live again once the LAST verdict has landed.
            // `spent` saturates one past the bound: `BudgetHeld` has already
            // been falsified there, and the witness keeps the graph finite.
            action Verdict when (inflight > 0) {
                inflight = inflight - 1;
                spent = if act == 4 && spent <= Budget { spent + 1 } else { spent };
                dwell = if act == 4 { 1 } else { dwell };
                phase = if inflight == 1 { 1 } else { phase };
            }

            action DwellElapses when (phase == 1 && dwell == 1) {
                dwell = 0;
            }

            // A turn succeeded again, or the vendor's own flow completed. The
            // generation is over: its last decision goes with it, and the
            // budget and the dwell carry into the next one.
            action Cleared when (phase == 1) {
                phase = 0;
                act = 0;
            }

            // The generation changed under the state: everything pending is
            // dropped, and the budget and the dwell carry into the next one.
            action GenerationChanged when (gen == 0) {
                gen = gen + 1;
                phase = 0;
                inflight = 0;
                act = 0;
            }

            // -- the class moves, and the ladder cursor moves with it --

            // A `StopFailure.error` outside the closed list forces `unknown`,
            // whatever else agrees. The `unknown` row is one entry long, so
            // the cursor has to go back to its beginning for `escalate` — the
            // one thing that row carries — to be reachable at all.
            // DEFECT: the class flips and the cursor stays where the old
            // ladder left it, the one-entry row is walked past its end, and
            // the generation answers `refused:exhausted` for ever with no
            // human told.
            action UnplaceableHookValue when (
                phase == 1 && inflight == 0 && class <= 6 && broke == 0
            ) {
                class = 7;
                act = 0;
                silent = if Buggy == 1 { 1 } else { silent };
                broke = if Buggy == 1 { 1 } else { broke };
            }

            // The same generation classified again as a DIFFERENT class —
            // money, here. The state adopts it and that row starts over, so
            // the decision that stands is the new row's.
            // DEFECT: the reclassification is dropped and the old row keeps
            // deciding, which is an automatic action running for a generation
            // whose latest evidence says `billing_error`.
            action ReclassifiedAsSpendBilling when (
                phase == 1 && inflight == 0 && class <= 4 && broke == 0
            ) {
                class = 5;
                act = if Buggy == 1 { act } else { 0 };
                broke = if Buggy == 1 { 1 } else { broke };
            }

            // -- the defects; every healthy branch changes nothing --

            // The engine answers `refused:in-flight` and queues.
            action SecondActionWhileInFlight when (phase == 2 && inflight == 1) {
                inflight = if Buggy == 1 { inflight + 1 } else { inflight };
            }

            // `network-offline`, `unknown` and `model-bucket-limit` may never
            // name a `switch-*`; the table refuses such a row at `config set`.
            action SwitchInForbiddenClass when (
                phase == 1 && inflight == 0 && broke == 0 &&
                (class == 1 || class == 4 || class == 7)
            ) {
                act = if Buggy == 1 { 4 } else { act };
                broke = if Buggy == 1 { 1 } else { broke };
            }

            // `unknown` may never name `retry`: a re-submit into a failure
            // nothing could place is a loop.
            action RetryInUnknownClass when (
                phase == 1 && inflight == 0 && broke == 0 && class == 7
            ) {
                act = if Buggy == 1 { 3 } else { act };
                broke = if Buggy == 1 { 1 } else { broke };
            }

            // `spend-billing` is pinned to `escalate` and `auth` to `relogin`
            // or `escalate`: money and a login are a human's call.
            action ActOutsideThePinnedRow when (
                phase == 1 && inflight == 0 && broke == 0 &&
                (class == 5 || class == 6)
            ) {
                act = if Buggy == 1 {
                    if class == 5 { 2 } else { 4 }
                } else {
                    act
                };
                broke = if Buggy == 1 { 1 } else { broke };
            }

            // An escalated generation is the human's until a human or the
            // vendor's own completed flow ends it. No timer resumes it.
            action ResumeEscalated when (phase == 4 && broke == 0) {
                phase = if Buggy == 1 { 1 } else { phase };
                auto = if Buggy == 1 { 1 } else { auto };
                broke = if Buggy == 1 { 1 } else { broke };
            }

            // A request carrying a generation the state no longer has is
            // refused whatever it asks for.
            action StaleGenerationRequest when (broke == 0) {
                stale = if Buggy == 1 { 1 } else { stale };
                broke = if Buggy == 1 { 1 } else { broke };
            }

            // The space guard. `spent` is deliberately absent: what bounds it
            // is `BudgetHeld`, a claim, not the shape of the state.
            invariant Bounds:
                class <= 7 && phase <= 4 && act <= 6 && inflight <= 2 &&
                dwell <= 1 && gen <= 1 && stale <= 1 && auto <= 1 &&
                silent <= 1 && broke <= 1;
            invariant OneInFlight: inflight <= 1;
            invariant BudgetHeld: spent <= Budget;
            invariant SwitchOnlyWhereTheRowAllowsIt:
                if act == 4 {
                    class == 0 || class == 2 || class == 3
                } else {
                    act <= 6
                };
            invariant RetryNeverForUnknown:
                if act == 3 { class <= 3 } else { act <= 6 };
            invariant PinnedRowsHold:
                if class == 5 {
                    act == 0 || act == 6
                } else if class == 6 {
                    act == 0 || act == 5 || act == 6
                } else {
                    act <= 6
                };
            invariant EscalationIsNeverLeftAutomatically: auto == 0;
            invariant StaleGenerationNeverActs: stale == 0;
            // Every row ends in `escalate`, so every generation can still
            // reach a human; a cursor left behind by a class change is what
            // would take that away.
            invariant AClassChangeNeverSilencesTheLadder: silent == 0;
        }
    }
}

/// A bounded harness capture may time out and reap its direct child while an
/// escaped descendant still holds stdout or stdin open. The capture only
/// returns after the reader and writer workers have both finished or been
/// cancelled and joined. Tier-1 runs the real `align::capture_bounded` with
/// escaped descendants and observes both worker completions at return.
/// `Buggy=1` recovers the historical early-return shape: child exit alone is
/// treated as enough, leaking a worker past the caller's deadline.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_capture_worker_lifecycle_model() -> Model {
    crate::ty_model! {
        HarnessCaptureWorkerLifecycle {
            const Buggy = 0;
            var child_reaped = 0;
            var reader_done = 0;
            var writer_done = 0;
            var deadline = 0;
            var returned = 0;

            action ChildExit when (child_reaped == 0 && returned == 0) {
                child_reaped = 1;
            }
            // The direct child may have exited while an escaped descendant
            // still holds a pipe. Its worker deadline remains meaningful.
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
            action WriterFinish when (writer_done == 0 && returned == 0) {
                writer_done = 1;
            }
            action Return when (
                returned == 0 && child_reaped == 1 &&
                ((reader_done == 1 && writer_done == 1) || Buggy == 1)
            ) {
                returned = 1;
            }

            invariant NoReturnBeforeWorkersJoin:
                returned == 0 ||
                (child_reaped == 1 && reader_done == 1 && writer_done == 1);
        }
    }
}

/// A window checks for a ready Claude upgrade five times at two-second
/// intervals after launch, even when an early socket/roster read succeeds or
/// fails. Subsequent checks use the minute cadence. The shipping `HostCadence`
/// projects its saturated tick and chosen delay onto this model in Tier-1.
/// `Buggy=1` retires the short cadence after an early successful read, the
/// defect that lets a Claude tab opened seconds later wait another minute.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_startup_cadence_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeStartupCadence {
            const Buggy = 0;
            const ShortTicks = 5;
            const Saturation = 6;
            var tick = 0;
            var short = 0;
            var ready = 0;

            action Ready when (tick == 1 && ready == 0) {
                ready = 1;
            }

            action Step when (tick <= Saturation) {
                short = if tick <= ShortTicks - 1 &&
                    (Buggy == 0 || ready == 0 || tick > 1) { 1 } else { 0 };
                tick = if tick <= Saturation - 1 { tick + 1 } else { tick };
            }

            invariant ShortUntilBudget:
                tick == 0 || tick > ShortTicks || short == 1;
        }
    }
}

/// A READY answer is usable only by the process and tab that received the
/// notice, with a unique live owner of that conversation and a complete
/// session-file scan. The process-start token represents the kernel PID-reuse
/// guard. `Buggy=1` replays the old conversation-keyed reducer, which accepts
/// READY in another tab or after a partial scan.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_notice_owner_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeNoticeOwner {
            const Buggy = 0;
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
            var signaled = 0;

            action Announce when (phase == 0 && owners == 1 && scan_complete == 1) {
                phase = 1;
                owner_tab = tab;
                owner_pid = pid;
                owner_start = start;
            }
            action Ready when (phase == 1 && ready == 0) {
                ready = 1;
            }
            action OtherTab when (phase == 1 && tab == 1) {
                tab = 2;
                pid = 2;
                start = 2;
            }
            action DuplicateOwner when (phase == 1 && owners == 1) {
                owners = 2;
            }
            action IncompleteScan when (scan_complete == 1 && phase <= 1) {
                scan_complete = 0;
            }
            action Terminate when (
                phase == 1 && ready == 1 &&
                (Buggy == 1 ||
                    (owners == 1 && scan_complete == 1 && owner_tab == tab &&
                     owner_pid == pid && owner_start == start))
            ) {
                phase = 2;
                signaled = 1;
            }

            invariant OnlyIssuerSignaled:
                signaled == 0 ||
                (owner_tab == tab && owner_pid == pid && owner_start == start);
            invariant NoDuplicateOwnerSignal: signaled == 0 || owners == 1;
            invariant NoPartialScanSignal: signaled == 0 || scan_complete == 1;
        }
    }
}
