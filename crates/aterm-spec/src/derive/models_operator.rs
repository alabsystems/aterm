// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded models for the resident fleet operator.
//!
//! These are deliberately scalar projections of the durable state machines
//! in `docs/OPERATOR-EMBEDDED.md`. The shipping service binds its concrete
//! transitions to these models at Tier 1; this crate owns the derived Tier-0
//! specifications and their executable semantics.

use super::*;

/// Durable attention-event delivery with opaque claim generations, expiry,
/// reclaim, resolution, and a redelivery cap.
///
/// `phase` is `0 Queued`, `1 Delivered`, `2 Resolved`, `3 EscalationQueued`,
/// `4 EscalationDelivered`, or `5 InDoubt`. Tokens
/// are represented by small monotonic integers: the real implementation uses
/// opaque random values, while the safety property needs only freshness and
/// equality. A stale acknowledgement races a newer live claim in `StaleAck`.
/// The healthy CAS leaves the event untouched; `Buggy=1` accepts the stale token
/// and exposes the regression.
///
/// `RedeliveryCap=2` is the bounded projection of the shipping policy's larger
/// configurable cap. On the first expiry the event is requeued; on the second it
/// becomes a claimable escalation. That final human-facing delivery is not
/// recycled again: expiry moves it to in-doubt.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn operator_event_delivery_model() -> Model {
    crate::ty_model! {
        OperatorEventDelivery {
            const Buggy = 0;
            const RedeliveryCap = 2;
            const TokenCap = 4;

            // 0 Queued, 1 Delivered, 2 Resolved, 3 EscalationQueued,
            // 4 EscalationDelivered, 5 InDoubt.
            var phase = 0;
            var token = 0;
            var next_token = 1;
            var stale_token = 0;
            var expired = 0;
            var redeliveries = 0;
            var escalated = 0;
            var resolution_token = 0;
            var stale_regression = 0;
            var in_doubt = 0;

            action Claim when (
                phase == 0 && escalated == 0 && next_token <= TokenCap - 2
            ) {
                phase = 1;
                token = next_token;
                next_token = next_token + 1;
                expired = 0;
            }

            action Expire when (
                (phase == 1 || phase == 4) && expired == 0
            ) {
                expired = 1;
            }

            action ReclaimForRetry when (
                phase == 1 && expired == 1 &&
                redeliveries <= RedeliveryCap - 2
            ) {
                stale_token = token;
                token = 0;
                phase = 0;
                expired = 0;
                redeliveries = redeliveries + 1;
            }

            action ReclaimAsEscalation when (
                phase == 1 && expired == 1 &&
                redeliveries == RedeliveryCap - 1
            ) {
                stale_token = token;
                token = 0;
                phase = 3;
                expired = 0;
                redeliveries = redeliveries + 1;
                escalated = 1;
            }

            action ResolveCurrent when (phase == 1 && expired == 0) {
                phase = 2;
                resolution_token = token;
            }

            action ClaimEscalation when (
                phase == 3 && escalated == 1 &&
                next_token <= TokenCap - 1
            ) {
                phase = 4;
                token = next_token;
                next_token = next_token + 1;
                expired = 0;
            }

            action ResolveEscalation when (phase == 4 && expired == 0) {
                phase = 2;
                resolution_token = token;
            }

            action ExpiredEscalationInDoubt when (
                phase == 4 && expired == 1
            ) {
                phase = 5;
                expired = 0;
                in_doubt = 1;
            }

            // Repeating the same token+resolution is an idempotent success.
            action AckSame when (
                phase == 2 && token > 0 && resolution_token == token
            ) {
                phase = phase;
            }

            // The stale token belongs to the reclaimed claim, while `token`
            // belongs to its successor. The healthy atomic CAS is a no-op.
            action StaleAck when (
                phase == 1 && stale_token > 0 && token > stale_token
            ) {
                phase = if Buggy == 1 { 2 } else { phase };
                resolution_token = if Buggy == 1 {
                    stale_token
                } else {
                    resolution_token
                };
                stale_regression = if Buggy == 1 { 1 } else { stale_regression };
            }

            invariant Bounds:
                phase <= 5 && token <= TokenCap && next_token <= TokenCap &&
                stale_token <= TokenCap && expired <= 1 &&
                redeliveries <= RedeliveryCap && escalated <= 1 &&
                resolution_token <= TokenCap && stale_regression <= 1 &&
                in_doubt <= 1;
            invariant ClaimStateOwnsToken:
                if phase == 0 || phase == 3 { token == 0 } else { token > 0 };
            invariant ExpiryBelongsToDeliveredClaim:
                if expired == 1 { phase == 1 || phase == 4 } else { expired == 0 };
            invariant ResolutionUsesCurrentToken:
                if phase == 2 {
                    resolution_token == token && token > 0
                } else {
                    resolution_token == 0
                };
            invariant EscalationOccursAtCap:
                if escalated == 1 {
                    redeliveries == RedeliveryCap && phase > 1
                } else {
                    phase <= 2
                };
            invariant InDoubtOnlyAfterEscalation:
                if phase == 5 { in_doubt == 1 && escalated == 1 } else { in_doubt == 0 };
            invariant StaleTokenCannotRegressState: stale_regression == 0;
        }
    }
}

/// The guarded actuator's write-ahead transaction.
///
/// `phase` is `0 Idle`, `1 IntentDurable`, `2 Mutated`, `3 InDoubt`, or
/// `4 Resolved`. The shipping `FinishAction` WAL frame atomically persists the
/// result and resolves the event, so `PersistResult` deliberately performs both
/// abstract updates in one action. `input_epoch` is the real sink's attempted-
/// input epoch and `expected_epoch` is the token returned by the actuator's own
/// paste. A foreign attempt between paste and submit advances only the former;
/// healthy `RejectInterjectedSubmit` emits no submit and moves the durable intent
/// in-doubt. Both unknown-outcome paths after a durable intent recover
/// conservatively to `InDoubt`; neither may replay the mutation. `Buggy=1`
/// `authority_valid` projects the host's final actuation permit: fleet fault,
/// unmanagement, and normal shutdown all revoke it under the same mutex held
/// through the bounded sink write. `Buggy=1` reproduces input across either an
/// interjection or revoked authority, plus an in-doubt replay.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn operator_wal_actuator_model() -> Model {
    crate::ty_model! {
        OperatorWalActuator {
            const Buggy = 0;

            // 0 Idle, 1 IntentDurable, 2 Mutated, 3 InDoubt, 4 Resolved.
            var phase = 0;
            var intent_durable = 0;
            var mutations = 0;
            var result_durable = 0;
            var in_doubt = 0;
            var resolved = 0;
            var replayed = 0;
            var input_epoch = 0;
            var expected_epoch = 0;
            var interjected = 0;
            var submit_writes = 0;
            var authority_valid = 1;
            var authority_invalidated = 0;
            var writes_after_invalidation = 0;

            action PersistIntent when (phase == 0) {
                phase = 1;
                intent_durable = 1;
            }

            action MutateOnce when (
                phase == 1 && intent_durable == 1 && authority_valid == 1
            ) {
                phase = 2;
                mutations = mutations + 1;
                // The successful conditional paste advances both the sink and
                // the actuator's carried token.
                input_epoch = input_epoch + 1;
                expected_epoch = expected_epoch + 1;
            }

            action ForeignInput when (
                phase == 2 && interjected == 0 && submit_writes == 0 &&
                input_epoch == expected_epoch && input_epoch <= 1
            ) {
                input_epoch = input_epoch + 1;
                interjected = 1;
            }

            action GuardedSubmit when (
                phase == 2 && mutations == 1 && interjected == 0 &&
                input_epoch == expected_epoch && submit_writes == 0 &&
                authority_valid == 1
            ) {
                submit_writes = 1;
            }

            // Fleet fault, unmanagement, and process shutdown share the same
            // final-write permit in the host. They can win before either the
            // paste (phase 1) or Enter (phase 2).
            action InvalidateAuthority when (
                (phase == 1 || phase == 2) && submit_writes == 0 &&
                authority_valid == 1
            ) {
                authority_valid = 0;
                authority_invalidated = 1;
            }

            // Healthy code returns a zero-byte conflict and the transaction
            // records in-doubt. The buggy branch witnesses one write after the
            // revocation and is rejected by AuthorityLossNeverEgresses.
            action RejectInvalidAuthority when (
                (phase == 1 || phase == 2) && submit_writes == 0 &&
                authority_valid == 0 && authority_invalidated == 1 &&
                writes_after_invalidation == 0
            ) {
                phase = if Buggy == 1 { phase } else { 3 };
                in_doubt = if Buggy == 1 { 0 } else { 1 };
                writes_after_invalidation = if Buggy == 1 { 1 } else { 0 };
            }

            // The healthy conditional compare rejects with zero submit bytes.
            // Buggy=1 models an actuator that ignores the stale epoch and emits
            // Enter against the interjected presentation.
            action RejectInterjectedSubmit when (
                phase == 2 && interjected == 1 &&
                input_epoch > expected_epoch && submit_writes == 0
            ) {
                phase = if Buggy == 1 { phase } else { 3 };
                submit_writes = if Buggy == 1 { 1 } else { 0 };
                in_doubt = if Buggy == 1 { 0 } else { 1 };
            }

            action PersistResult when (
                phase == 2 && mutations == 1 && submit_writes == 1
            ) {
                phase = 4;
                result_durable = 1;
                resolved = 1;
            }

            // Even the known pre-mutation crash is conservatively in-doubt on
            // replay: the recovered process observes durable records, not the
            // vanished process's instruction pointer.
            action CrashAfterIntent when (phase == 1) {
                phase = 3;
                in_doubt = 1;
            }

            action CrashAfterMutation when (phase == 2) {
                phase = 3;
                in_doubt = 1;
            }

            action ResolveInDoubt when (phase == 3 && in_doubt == 1) {
                phase = 4;
                resolved = 1;
            }

            // Recovery may inspect an in-doubt record, but it must not execute
            // its non-idempotent mutation again.
            action ReplayInDoubt when (
                phase == 3 && in_doubt == 1 && mutations > 0
            ) {
                // Saturate the bounded defect witness at two executions. The
                // invariant still catches the replay immediately, while the
                // Buggy=1 state graph remains finite for strict-vacuity and
                // deadlock sweeps that intentionally continue past the first
                // counterexample.
                mutations = if Buggy == 1 { 2 } else { mutations };
                replayed = if Buggy == 1 { 1 } else { replayed };
            }

            invariant Bounds:
                phase <= 4 && mutations <= 1 && intent_durable <= 1 &&
                result_durable <= 1 && in_doubt <= 1 && resolved <= 1 &&
                replayed <= 1 && input_epoch <= 2 && expected_epoch <= 1 &&
                interjected <= 1 && submit_writes <= 1 &&
                authority_valid <= 1 && authority_invalidated <= 1 &&
                writes_after_invalidation <= 1;
            invariant MutationRequiresDurableIntent:
                if mutations > 0 { intent_durable == 1 } else { mutations == 0 };
            invariant ResultFollowsOneSubmittedMutation:
                if result_durable == 1 {
                    mutations == 1 && submit_writes == 1
                } else {
                    result_durable == 0
                };
            invariant SubmitUsesCurrentEpoch:
                if submit_writes == 1 {
                    input_epoch == expected_epoch && interjected == 0
                } else {
                    submit_writes == 0
                };
            invariant InterjectionNeverSubmits:
                if interjected == 1 { submit_writes == 0 } else { interjected == 0 };
            invariant AuthorityStateIsExclusive:
                authority_valid + authority_invalidated == 1;
            invariant AuthorityLossNeverEgresses: writes_after_invalidation == 0;
            invariant DurableOutcomesAreExclusive: result_durable + in_doubt <= 1;
            invariant ResolutionHasDurableOutcome:
                if resolved == 1 {
                    result_durable == 1 || in_doubt == 1
                } else {
                    resolved == 0
                };
            invariant NeverReplayInDoubt: replayed == 0;
        }
    }
}

/// Full-snapshot discipline for the embedded observer's `(grid epoch, content
/// seq)` cursor.
///
/// The operator does not consume the external `subscribe` wire and therefore has
/// no GAP handshake. It re-reads a complete `Store` snapshot in-process. An alt
/// grid/lifecycle identity reset must atomically install that fresh snapshot and
/// its new cursor (`ResetAndResnapshot`). `ResetWithoutResnapshot` reproduces the
/// stale-evidence defect under `Buggy=1`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn operator_resync_cursor_model() -> Model {
    crate::ty_model! {
        OperatorResyncCursor {
            const Buggy = 0;
            const MaxEpoch = 3;
            const MaxSeq = 2;

            var source_epoch = 1;
            var source_seq = 0;
            var cursor_epoch = 1;
            var cursor_seq = 0;
            // Highest epoch installed from a complete Store snapshot.
            var snapshot_epoch = 1;
            var silent_loss = 0;

            action ObserveAdvance when (source_seq <= MaxSeq - 1) {
                source_seq = source_seq + 1;
                cursor_seq = source_seq + 1;
            }

            action ResetAndResnapshot when (source_epoch <= MaxEpoch - 1) {
                source_epoch = source_epoch + 1;
                source_seq = 0;
                cursor_epoch = source_epoch + 1;
                cursor_seq = 0;
                snapshot_epoch = source_epoch + 1;
            }

            action ResetWithoutResnapshot when (source_epoch <= MaxEpoch - 1) {
                source_epoch = if Buggy == 1 { source_epoch + 1 } else { source_epoch };
                source_seq = if Buggy == 1 { 0 } else { source_seq };
                silent_loss = if Buggy == 1 { 1 } else { silent_loss };
            }

            // A caught-up quiet stream may remain parked without changing its
            // cursor. This is the resident service's normal steady state.
            action ParkCurrent when (
                cursor_epoch == source_epoch &&
                cursor_seq == source_seq
            ) {
                cursor_seq = cursor_seq;
            }

            invariant Bounds:
                source_epoch <= MaxEpoch && cursor_epoch <= MaxEpoch &&
                snapshot_epoch <= MaxEpoch && source_seq <= MaxSeq && cursor_seq <= MaxSeq &&
                silent_loss <= 1;
            invariant CursorHasCurrentSnapshot:
                cursor_epoch == source_epoch && snapshot_epoch == source_epoch &&
                cursor_seq == source_seq;
            invariant NoSilentLossAcrossReset: silent_loss == 0;
        }
    }
}

/// Single-leader residency and run-epoch fencing across crash/takeover.
///
/// A kernel-released lock is projected as the `a_live`/`b_live` exclusion guard.
/// Each acquisition increments `epoch`; writes are accepted only from the live
/// holder carrying that epoch. Two hostile attempts remain explicit actions so
/// Tier-1 can bind their rejection: takeover while A still owns the lock, and an
/// old A write after B has taken over. At `Buggy=1` those attempts are accepted.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn operator_leadership_model() -> Model {
    crate::ty_model! {
        OperatorLeadership {
            const Buggy = 0;
            const MaxEpoch = 3;

            var a_live = 0;
            var b_live = 0;
            var epoch = 0;
            var a_epoch = 0;
            var b_epoch = 0;
            var stale_epoch = 0;
            var accepted_epoch = 0;
            var stale_accepted = 0;
            var overlap = 0;

            action StartA when (
                a_live == 0 && b_live == 0 && epoch <= MaxEpoch - 1
            ) {
                a_live = 1;
                epoch = epoch + 1;
                a_epoch = epoch + 1;
                accepted_epoch = 0;
            }

            action CurrentWriteA when (
                a_live == 1 && b_live == 0 && a_epoch == epoch
            ) {
                accepted_epoch = a_epoch;
            }

            action LoseA when (a_live == 1 && b_live == 0) {
                a_live = 0;
                stale_epoch = a_epoch;
                accepted_epoch = 0;
            }

            action TakeoverB when (
                a_live == 0 && b_live == 0 && stale_epoch > 0 &&
                epoch <= MaxEpoch - 1
            ) {
                b_live = 1;
                epoch = epoch + 1;
                b_epoch = epoch + 1;
                accepted_epoch = 0;
            }

            action CurrentWriteB when (
                a_live == 0 && b_live == 1 && b_epoch == epoch
            ) {
                accepted_epoch = b_epoch;
            }

            action LoseB when (a_live == 0 && b_live == 1) {
                b_live = 0;
                stale_epoch = b_epoch;
                accepted_epoch = 0;
            }

            // A failed liveness probe is not authority to steal a live lock.
            // Healthy behavior is refusal (no state change).
            action AttemptTakeoverWhileLive when (
                a_live == 1 && b_live == 0 && epoch <= MaxEpoch - 1
            ) {
                b_live = if Buggy == 1 { 1 } else { b_live };
                epoch = if Buggy == 1 { epoch + 1 } else { epoch };
                b_epoch = if Buggy == 1 { epoch + 1 } else { b_epoch };
                overlap = if Buggy == 1 { 1 } else { overlap };
            }

            // Once B owns a newer epoch, an old A process may still execute but
            // cannot append an accepted record under its stale epoch.
            action StaleWriteAfterTakeover when (
                a_live == 0 && b_live == 1 && stale_epoch > 0
            ) {
                accepted_epoch = if Buggy == 1 { stale_epoch } else { accepted_epoch };
                stale_accepted = if Buggy == 1 { 1 } else { stale_accepted };
            }

            invariant Bounds:
                a_live <= 1 && b_live <= 1 && epoch <= MaxEpoch &&
                a_epoch <= MaxEpoch && b_epoch <= MaxEpoch &&
                stale_epoch <= MaxEpoch && accepted_epoch <= MaxEpoch &&
                stale_accepted <= 1 && overlap <= 1;
            invariant SingleLeader: a_live + b_live <= 1 && overlap == 0;
            invariant LiveLeaderOwnsCurrentEpoch:
                if a_live == 1 {
                    b_live == 0 && a_epoch == epoch
                } else if b_live == 1 {
                    a_live == 0 && b_epoch == epoch
                } else {
                    a_live == 0 && b_live == 0
                };
            invariant AcceptedWriteUsesCurrentEpoch:
                accepted_epoch == 0 || accepted_epoch == epoch;
            invariant StaleEpochNeverAccepted: stale_accepted == 0;
        }
    }
}

/// Durable fleet-fault latch and explicit human clear protocol.
///
/// `phase` is `0 Healthy`, `1 MarkerPrepared`, `2 Faulted`, `3 Rebaseline`, or
/// `4 ClearCommittedMarkerPresent`. The marker is synchronized before the WAL
/// fault record, so a crash in phase 1 recovers to Faulted. Clear commits the
/// durable healthy transition before marker removal; a crash in phase 4 safely
/// re-latches from the retained marker. `Buggy=1` models an actuator that emits
/// while any of those fail-closed phases owns authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn operator_fleet_fault_model() -> Model {
    crate::ty_model! {
        OperatorFleetFault {
            const Buggy = 0;
            const Managed = 2;

            // 0 Healthy, 1 MarkerPrepared, 2 Faulted, 3 Rebaseline,
            // 4 ClearCommittedMarkerPresent.
            var phase = 0;
            var marker = 0;
            var pending = 0;
            var in_doubt = 0;
            var actions = 0;
            var blocked_egress = 0;

            action PrepareFault when (phase == 0 || phase == 3) {
                phase = 1;
                marker = 1;
                pending = 0;
            }

            // The public API holds the queue mutex across marker sync and WAL
            // commit, so callers observe this combined successful transition.
            action LatchFault when (phase == 0 || phase == 3) {
                phase = 2;
                marker = 1;
                pending = 0;
            }

            action CommitFault when (phase == 1 && marker == 1) {
                phase = 2;
            }

            action CrashAfterMarker when (phase == 1 && marker == 1) {
                phase = 2;
            }

            action BeginClear when (phase == 2 && marker == 1) {
                phase = 3;
                pending = Managed;
            }

            action BeginClearWithInDoubt when (phase == 2 && marker == 1) {
                phase = 3;
                pending = Managed;
                in_doubt = 1;
            }

            action BaselineOne when (phase == 3 && pending > 0) {
                pending = pending - 1;
            }

            action HumanReconcile when (phase == 3 && in_doubt == 1) {
                in_doubt = 0;
            }

            action CommitClear when (
                phase == 3 && marker == 1 && pending == 0 && in_doubt == 0
            ) {
                phase = 4;
            }

            action RemoveMarker when (phase == 4 && marker == 1) {
                phase = 0;
                marker = 0;
            }

            // Successful callers observe the commit+marker-removal pair while
            // the queue mutex prevents intervening actuator admission.
            action CompleteClear when (
                phase == 3 && marker == 1 && pending == 0 && in_doubt == 0
            ) {
                phase = 0;
                marker = 0;
            }

            action CrashAfterClearCommit when (phase == 4 && marker == 1) {
                phase = 2;
            }

            action ActuateHealthy when (
                phase == 0 && marker == 0 && actions == 0
            ) {
                actions = 1;
            }

            action AttemptActuateBlocked when (
                phase > 0 && marker == 1 && actions == 0 && blocked_egress == 0
            ) {
                actions = if Buggy == 1 { 1 } else { actions };
                blocked_egress = if Buggy == 1 { 1 } else { blocked_egress };
            }

            invariant Bounds:
                phase <= 4 && marker <= 1 && pending <= Managed &&
                in_doubt <= 1 && actions <= 1 && blocked_egress <= 1;
            invariant MarkerOwnsEveryBlockedPhase:
                if phase == 0 { marker == 0 } else { marker == 1 };
            invariant ClearCommitHasNoAmbiguity:
                if phase == 4 { pending == 0 && in_doubt == 0 } else { phase <= 3 };
            invariant FaultedActuatorCannotEgress: blocked_egress == 0;
        }
    }
}
