// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded models for the aterm wrapper's harness core
//! (`docs/DESIGN-aterm-wrapper-2026-09-17.md` §11 items 5 and 7, plus the
//! grid spine's turn machine of §4.2/§5.8.1).
//!
//! Three scalar projections of machines that ship in `aterm_agent::harness`:
//! the bounded, lossy ledger ring (`harness::ring`), the limit-recovery
//! engine (`harness::limits`) and the observer's turn machine
//! (`harness::observe`). As everywhere in this crate the model is
//! hand-written Rust DESCRIBING that code, not extracted from it: Tier 0 here
//! says the description holds over its whole bounded space, and only the
//! Tier-1 binds in `aterm-agent/tests/conformance_harness.rs` — which drive
//! the real `Ring`, the real engine and the real `Observer` and project their
//! state onto these variables — make any of them a statement about the
//! program that compiled.

use super::*;

/// The ledger ring's SLOT accounting (design §11 item 5).
///
/// Every line in a segment file occupies one id slot, whether it is a framed
/// row, sealed junk, or an unterminated fragment. `nextid` is the id the live
/// writer hands out next, `top` one past the last slot ON DISK, `lo` the first
/// slot still on disk, `segs` the segment files, `fill` the slots in the newest
/// one, `torn` that the newest segment ends in a fragment, and `live` that a
/// writer holds the ring.
///
/// `Emit` is an append that fits the active segment and `Rotate` one that
/// starts a new one (the name is `Emit`, never `Append`, which collides with
/// ty's Sequences builtin and yields "undefined Next"). `Tear`,
/// `CrashAfterCreate` and `Crash` are the three ways a writer dies that the
/// real ring's reopen must survive; `Reopen` is what it then finds.
///
/// `Cap` is `SegCap * Segments` written out, because the expression language
/// has no multiplication, and `MaxId` is the exploration bound rather than a
/// property of the ring.
///
/// `Buggy = 1` arms three historical defect shapes at once, one per design
/// claim: a reopen that does not count the torn tail's slot and so hands that
/// id out a SECOND time (`Reopen`), a rotation that forgets the second-oldest
/// segment as well as the oldest (`RotateForgettingTwo`), and a rotation that
/// forgets nothing and so leaves the ring over its bound with one file too many
/// (`RotateForgettingNothing`). The two rotation defects are separate actions
/// whose healthy branch changes nothing, so the committed machine is exactly
/// the ring's real behaviour and the mutants sit beside it; `forged` saturates
/// the witness so the `Buggy = 1` state graph stays finite.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_ledger_ring_model() -> Model {
    crate::ty_model! {
        HarnessLedgerRing {
            const Buggy = 0;
            const SegCap = 2;
            const Segments = 3;
            // SegCap * Segments: the macro has no multiplication.
            const Cap = 6;
            // The exploration bound, not a property of the ring.
            const MaxId = 9;

            var nextid = 1;
            var top = 1;
            var lo = 1;
            var segs = 0;
            var fill = 0;
            var torn = 0;
            var live = 1;
            var forged = 0;

            action Emit when (
                live == 1 && nextid <= MaxId && segs > 0 && fill <= SegCap - 1
            ) {
                nextid = nextid + 1;
                top = top + 1;
                fill = fill + 1;
                torn = 0;
            }

            // A record that does not fit starts a segment, and the oldest is
            // unlinked BEFORE the record is written once the ring is full.
            action Rotate when (
                live == 1 && nextid <= MaxId && (segs == 0 || fill == SegCap)
            ) {
                nextid = nextid + 1;
                top = top + 1;
                fill = 1;
                torn = 0;
                segs = if segs <= Segments - 1 { segs + 1 } else { segs };
                lo = if segs <= Segments - 1 { lo } else { lo + SegCap };
            }

            // Death mid-write: the record's first bytes reached the segment.
            action Tear when (
                live == 1 && nextid <= MaxId && segs > 0 && fill <= SegCap - 1
            ) {
                top = top + 1;
                fill = fill + 1;
                torn = 1;
                live = 0;
            }

            // Death after the new segment was created, before the oldest was
            // unlinked: one file too many, and the newest is empty.
            action CrashAfterCreate when (
                live == 1 && nextid <= MaxId && (segs == 0 || fill == SegCap)
            ) {
                segs = segs + 1;
                fill = 0;
                torn = 0;
                live = 0;
            }

            action Crash when (live == 1) {
                live = 0;
            }

            // The reopen writes nothing: it counts the slots it finds. The
            // torn tail's slot is one of them, so the fragment's id is burned
            // however many times the ring is reopened.
            action Reopen when (live == 0) {
                live = 1;
                nextid = if Buggy == 1 { top - torn } else { top };
                segs = if segs <= Segments { segs } else { segs - 1 };
                lo = if segs <= Segments { lo } else { lo + SegCap };
            }

            // DEFECT: a rotation that forgets the second-oldest segment too.
            // The healthy branch changes nothing, which is what the real
            // `make_room` does at this state.
            action RotateForgettingTwo when (
                live == 1 && segs == Segments && fill == SegCap && forged == 0
            ) {
                lo = if Buggy == 1 { lo + SegCap + SegCap } else { lo };
                segs = if Buggy == 1 { segs - 1 } else { segs };
                forged = if Buggy == 1 { 1 } else { forged };
            }

            // DEFECT: a rotation that forgets nothing, leaving the ring over
            // its byte bound with one segment file too many while live.
            action RotateForgettingNothing when (
                live == 1 && segs == Segments && fill == SegCap && forged == 0
            ) {
                segs = if Buggy == 1 { segs + 1 } else { segs };
                top = if Buggy == 1 { top + 1 } else { top };
                nextid = if Buggy == 1 { nextid + 1 } else { nextid };
                fill = if Buggy == 1 { 1 } else { fill };
                forged = if Buggy == 1 { 1 } else { forged };
            }

            // Ids only grow, and a slot that exists on disk is never handed
            // out again — the torn tail's slot included.
            invariant NoReuse: live == 0 || top <= nextid;
            // The ring holds at most one ring's worth of slots.
            invariant Bounded: top - lo <= Cap;
            // A rotation drops the OLDEST segment and no more: either nothing
            // was ever dropped, or all but one segment's slots are still held.
            invariant OnlyOldestLost: lo == 1 || Cap - SegCap <= top - lo;
            // One file too many is a crash shape a reopen trims, never a state
            // a live writer is in.
            invariant FilesBounded:
                segs <= Segments + 1 && (live == 0 || segs <= Segments);
            // The space guard: a segment never holds more slots than it can.
            invariant FillBounded: fill <= SegCap;
        }
    }
}

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

/// The grid spine's TURN machine (design §4.2, §5.8.1; `harness::observe`).
///
/// A writer/reader pair with one law. The WRITER is the evidence that opens a
/// turn — a parsed grid whose worker phase is busy (or blocked at an approval
/// box), or, where no grid was read this pass, `status phase=running` alone.
/// The READER is the evidence that may close it. They are not symmetric, and
/// the asymmetry is the whole anti-flap rule:
///
/// > **A turn opened by the GRID may be closed by an exit or by a later grid
/// > read, but NEVER by `status` alone** — because a pass that read no grid
/// > did not look, and "did not look" is not "not busy".
///
/// Without it, a pass whose `revision` did not move (so the grid was
/// deliberately not re-read — the `needs_grid` gate that makes an idle
/// session cost nothing) would close a live turn from the weaker source and
/// re-open it on the next grid read, flapping `turn-began`/`turn-ended` at
/// the ledger and at every actuator gated on a turn boundary.
///
/// `turn` is the source that opened the turn in flight, in the coding of
/// `harness::observe`'s turn source (`harness::source::Source`): `0` none, `1` grid-opened, `2`
/// status-opened. `exited` is that the session's program has been seen to go.
/// `statusclosed` is a WITNESS the shipping observer never sets: a
/// grid-opened turn closed by a pass that read no grid.
///
/// The transitions are the shipped `on_sample` arms, one each: the grid opens
/// (`(None, true)` with grid rows), `status` opens where no grid was read,
/// a grid reading UPGRADES a status-opened turn to grid-opened (the
/// `(Some(_), true)` arm — it emits no event, which is why the Tier-1 bind
/// reads `Observer::turn_source` rather than the event stream), either source
/// closes a turn it is allowed to close, and the exit closes whatever is open
/// under it before the `exited` event.
///
/// `ExitedSampleRepeats` is a later sample of an already-exited session — the
/// observer's `!self.exited` guard, which is why `exited` is emitted once.
/// `NewSession` is the host building a fresh `Observer` for the next session.
/// It is in the machine so the bounded graph has no wedge at `exited == 1`;
/// the shipping observer itself never un-exits.
///
/// `Buggy = 1` arms exactly two defect shapes, one per law: `status` closing a
/// grid-opened turn (`StatusWouldCloseAGridTurn` — the healthy branch changes
/// nothing, which is what the real arm does), and an exit that leaves the open
/// turn behind instead of closing it under itself.
///
/// Deliberately absent: banners, `output-moved`, `quiet`, confidence,
/// provenance and the program-naming path. Those are per-sample derivations
/// with no state that survives a pass; this machine states the ONE law about
/// state that does.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_turn_observation_model() -> Model {
    crate::ty_model! {
        HarnessTurnObservation {
            const Buggy = 0;

            var turn = 0;
            var exited = 0;
            var statusclosed = 0;

            // The grid says busy (or an approval box, which has NOT ended the
            // turn): the strong source opens it.
            action GridOpensTurn when (exited == 0 && turn == 0) {
                turn = 1;
            }

            // No grid this pass — `revision` did not move, or the read failed
            // — and `status` says running. The weaker source may OPEN.
            action StatusOpensTurn when (exited == 0 && turn == 0) {
                turn = 2;
            }

            // A later pass DID read the grid and it is still busy: the open
            // turn's source is upgraded, so the grid may close it later. No
            // event is emitted for this.
            action GridUpgradesTheSource when (exited == 0 && turn == 2) {
                turn = 1;
            }

            // The grid was read and the worker is at its composer: whichever
            // source opened the turn, the strong one closes it.
            action GridClosesTurn when (exited == 0 && turn > 0) {
                turn = 0;
            }

            // `status` closes a turn `status` opened. This is the ONLY
            // status-sourced close.
            action StatusClosesItsOwnTurn when (exited == 0 && turn == 2) {
                turn = 0;
            }

            // DEFECT: the same pass against a GRID-opened turn. The healthy
            // branch changes nothing — the real arm falls through with a
            // comment — so the committed machine is the observer's behaviour
            // and the mutant sits beside it.
            action StatusWouldCloseAGridTurn when (
                exited == 0 && turn == 1 && statusclosed == 0
            ) {
                turn = if Buggy == 1 { 0 } else { turn };
                statusclosed = if Buggy == 1 { 1 } else { statusclosed };
            }

            // The program is gone. Any open turn is closed UNDER the exit,
            // whatever opened it — an exit is not "did not look".
            // DEFECT: the exit is emitted and the turn is left in flight.
            action SessionExited when (exited == 0) {
                exited = 1;
                turn = if Buggy == 1 { turn } else { 0 };
            }

            // A later sample of an already-exited session. The `exited` event
            // is emitted ONCE, nothing reopens, and the machine holds still.
            action ExitedSampleRepeats when (exited == 1) {
                turn = 0;
            }

            // The host builds a fresh observer for the next session.
            action NewSession when (exited == 1) {
                exited = 0;
                turn = 0;
            }

            // The space guard.
            invariant Bounds: turn <= 2 && exited <= 1 && statusclosed <= 1;
            // THE law. A pass that did not look never ends a turn the grid
            // opened.
            invariant AGridTurnIsNeverClosedByStatusAlone: statusclosed == 0;
            // The exit closes what it finds: no turn outlives the program
            // that was running it.
            invariant NoTurnSurvivesTheExit:
                if exited == 1 { turn == 0 } else { turn <= 2 };
        }
    }
}
