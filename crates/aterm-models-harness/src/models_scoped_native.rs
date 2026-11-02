// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// An actual native constructor shares input ordering and retains its one admitted operation
/// across transition publication, EOF and ambiguous ACKs. Recovery never creates a mutation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn scoped_native_handoff_model() -> Model {
    aterm_spec::ty_model! {
        ScopedNativeHandoff {
            const Buggy = 0;
            var phase = 0; var old_input = 1; var native = 0; var mutation = 0;
            var fresh_input = 0; var ack = 0; var unauthorized = 0; var recreated = 0;
            var records = 0; var decision = 0;
            var disabled_native = 0;
            action DisabledConstructor when (phase == 0) {
                phase = 14; disabled_native = if Buggy == 10 {1} else {0};
            }
            action WrongActor when (phase == 0) {
                phase = 12; unauthorized = if Buggy == 1 {1} else {0};
            }
            action Reserve when (phase == 0) { phase = 1; }
            action Overtake when (phase == 1) {
                phase = 2; native = if Buggy == 2 {1} else {0};
            }
            action OldInputFinish when (phase == 2) { phase = 3; old_input = 0; }
            action NativeStart when (phase == 3) { phase = 4; native = 1; mutation = 1; }
            action Publish when (phase == 4) { phase = 5; }
            action EarlyAck when (phase == 5) { phase = 6; ack = if Buggy == 3 {1} else {0}; }
            action NewRevisionInput when (phase == 6) {
                phase = 7; fresh_input = if Buggy == 4 {1} else {0};
            }
            action LoseAck when (phase == 7) {
                phase = 8; native = if Buggy == 5 {0} else {1};
            }
            action Recover when (phase == 8) {
                phase = 9; mutation = if Buggy == 6 {2} else {1};
            }
            action NewHost when (phase == 9) {
                phase = 10; recreated = if Buggy == 7 {1} else {0};
            }
            action KernelOutcome when (phase == 10) {
                phase = 11; native = 0; ack = 1; records = 1; decision = 1;
            }
            action DuplicateCompletion when (phase == 11) {
                phase = 13; records = if Buggy == 8 {2} else {1};
                decision = if Buggy == 9 {2} else {1};
            }
            invariant ExactActorScopeRevision: unauthorized == 0;
            invariant NativeCannotOvertakeInput: old_input == 0 || native == 0;
            invariant TransitionAckWaitsActualCustody: ack == 0 || (old_input == 0 && native == 0);
            invariant NewRevisionCannotStarveHandoff: fresh_input == 0;
            invariant LostAckRetainsCustody: 8 > phase || phase > 10 || native == 1;
            invariant RecoveryNeverMutatesTwice: 2 > mutation;
            invariant NewHostCannotRecreateOperation: recreated == 0;
            invariant CompletionIsRecordedOnce: 2 > records;
            invariant TerminalOutcomeIsImmutable: 2 > decision;
            invariant ConstructorCapabilityPrecedesAdmission: disabled_native == 0;
        }
    }
}

/// Closing an ungranted forward slot is not an extinction claim. A fixed rollback waits
/// the old input prefix, and a lost Go acknowledgement keeps its exact watch until death.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_restart_slots_model() -> Model {
    aterm_spec::ty_model! {
        NativeRestartSlots {
            const Buggy = 0;
            var phase = 0; var pending = 0; var closed = 0; var rollback = 0;
            var late_go = 0; var renewed = 0; var granted = 0; var watch = 0;
            var alive = 1; var retired = 0;
            action ForwardQueued when (phase == 0) { phase = 1; pending = 1; }
            action PendingClose when (phase == 1) {
                phase = 2; rollback = if Buggy == 1 {1} else {0};
            }
            action ForwardDrain when (phase == 2) { phase = 3; pending = 0; }
            action CloseNoGo when (phase == 3) { phase = 4; closed = 1; }
            action LateForwardGo when (phase == 4) {
                phase = 5; late_go = if Buggy == 2 {1} else {0};
            }
            action FixedRollback when (phase == 5) { phase = 6; rollback = 1; }
            action RenewBudget when (phase == 6) {
                phase = 7; renewed = if Buggy == 3 {1} else {0};
            }
            action UnknownGo when (phase == 7) {
                phase = 8; granted = 1; watch = if Buggy == 4 {0} else {1};
            }
            action EarlyRetirement when (phase == 8) {
                phase = 9; retired = if Buggy == 5 {1} else {0};
            }
            action ChildDeath when (phase == 9) { phase = 10; alive = 0; }
            action Retire when (phase == 10) { phase = 11; retired = 1; }
            invariant RollbackWaitsPrefix: pending == 0 || rollback == 0;
            invariant ClosedSlotNeverGrantsAgain: closed == 0 || late_go == 0;
            invariant OriginalBudgetIsImmutable: renewed == 0;
            invariant LostGoRetainsWatch: granted == 0 || watch == 1;
            invariant GrantedChildRetiresOnlyAfterDeath: retired == 0 || alive == 0;
        }
    }
}

/// An initial preparation may be delayed before publication. Its cancellation seals the
/// same bounded id slot; final insertion and expired replay cannot create a late action.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn initial_prepare_cancellation_model() -> Model {
    aterm_spec::ty_model! {
        InitialPrepareCancellation {
            const Buggy = 0;
            var phase = 0; var sealed = 0; var admitted = 0; var expired = 0;
            var mutating = 0; var rollback_safe = 0; var slots = 0; var old_image = 1;
            action Reserve when (phase == 0) { phase = 1; slots = 1; }
            action CancelBeforePublication when (phase == 1) {
                phase = 2; sealed = 1; rollback_safe = 1;
            }
            action DelayedPublication when (phase == 2) {
                phase = 3; admitted = if Buggy == 1 {1} else {0};
            }
            action OriginalDeadline when (phase == 3) { phase = 4; expired = 1; }
            action ReclaimedSealReplay when (phase == 4) {
                phase = 5; admitted = if Buggy == 2 {1} else {0};
            }
            action Committing when (phase == 0) { phase = 6; mutating = 1; }
            action CancelUnresolved when (phase == 6) {
                phase = 7; rollback_safe = if Buggy == 3 {1} else {0};
            }
            action FullHistory when (phase == 0) { phase = 8; slots = 128; }
            action CancelAtCapacity when (phase == 8) {
                phase = 9; slots = if Buggy == 4 {129} else {128};
            }
            action ReclaimedCommittedHistory when (phase == 0) { phase = 10; old_image = 0; }
            action CancelFromTargetImage when (phase == 10) {
                phase = 11; rollback_safe = if Buggy == 5 {1} else {0};
            }
            invariant CancelPreventsLateAdmission: sealed == 0 || admitted == 0;
            invariant ExpiredOriginalCannotRecreate: expired == 0 || admitted == 0;
            invariant UnknownMutationNeverProvesRollback: mutating == 0 || rollback_safe == 0;
            invariant CancellationHistoryIsBounded: 129 > slots;
            invariant ReclaimedHistoryNeedsOriginalActor: old_image == 1 || rollback_safe == 0;
        }
    }
}
