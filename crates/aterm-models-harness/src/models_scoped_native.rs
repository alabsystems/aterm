// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A deferred restart resolves once from its exact child context, independently of the
/// immutable preparation. Context includes the claim, person sequence, authenticated peer,
/// process birth/image and host epoch; a changed component cannot supply execution authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn deferred_restart_resolution_model() -> Model {
    aterm_spec::ty_model! {
        DeferredRestartResolution {
            const Buggy = 0;
            var phase = 0; var intent = 1; var resolved = 0;
            var child = 0; var facts = 0; var policy = 0;
            var context = 1; var revoked = 0; var expired = 0;
            var selected_binding = 0; var current_binding = 1;
            var executions = 0; var receipt = 0; var result = 0;
            action Prepare when (phase == 0) { phase = 1; }
            action ExecuteUnresolved when (phase == 1) {
                phase = 2; executions = if Buggy == 1 {1} else {0};
            }
            action ChildReady when (phase == 2) { phase = 30; child = 1; }
            action ObserveHelper when (phase == 30) { phase = 3; facts = 1; policy = 1; }
            action ObserveEmptyHelper when (phase == 30) { phase = 3; facts = 1; policy = 1; }
            action Resolve when (phase == 3) {
                phase = 4; resolved = 1; selected_binding = 1;
                intent = if Buggy == 2 {2} else {1};
            }
            action DuplicateResolve when (phase == 4) {
                phase = 5; resolved = if Buggy == 3 {2} else {1};
            }
            action Execute when (phase == 5) { phase = 6; executions = 1; receipt = 1; }
            action DuplicateExecute when (phase == 6) {
                phase = 7; executions = if Buggy == 7 {2} else {1};
            }
            action ForeignResult when (phase == 7) {
                phase = 8; result = if Buggy == 8 {2} else {0};
            }
            action MatchingResult when (phase == 8) { phase = 9; result = 1; }
            action Revoke when (phase == 4) { phase = 10; revoked = 1; }
            action ExecuteRevoked when (phase == 10) {
                phase = 11; executions = if Buggy == 4 {1} else {0};
            }
            action Deadline when (phase == 4) { phase = 12; expired = 1; }
            action ExecuteExpired when (phase == 12) {
                phase = 13; executions = if Buggy == 5 {1} else {0};
            }
            action ReplaceContext when (phase == 4) { phase = 14; context = 2; }
            action ExecuteReplaced when (phase == 14) {
                phase = 15; executions = if Buggy == 6 {1} else {0};
            }
            action LoseAck when (phase == 6) { phase = 16; }
            action Recover when (phase == 16) {
                phase = 17;
                resolved = if Buggy == 9 {2} else {1};
                executions = if Buggy == 10 {2} else {1};
            }
            action OtherChild when (phase == 3) { phase = 18; child = 2; }
            action ResolveOtherChild when (phase == 18) {
                phase = 19; resolved = if Buggy == 11 {1} else {0};
            }
            action RevokeBeforeResolve when (phase == 3) { phase = 20; revoked = 1; }
            action ResolveRevoked when (phase == 20) {
                phase = 21; resolved = if Buggy == 12 {1} else {0};
            }
            action DeadlineBeforeResolve when (phase == 3) { phase = 22; expired = 1; }
            action ResolveExpired when (phase == 22) {
                phase = 23; resolved = if Buggy == 13 {1} else {0};
            }
            action ReplaceBeforeResolve when (phase == 3) { phase = 24; context = 2; }
            action ResolveReplaced when (phase == 24) {
                phase = 25; resolved = if Buggy == 14 {1} else {0};
            }
            action OtherReceipt when (phase == 6) { phase = 26; receipt = 2; }
            action ResultFromOtherReceipt when (phase == 26) {
                phase = 27; result = if Buggy == 15 {1} else {0};
            }
            action UnknownChild when (phase == 2) { phase = 28; }
            action ResolveUnknownChild when (phase == 28) {
                phase = 29; resolved = if Buggy == 16 {1} else {0};
            }
            action RefusedPolicy when (phase == 30) { phase = 31; facts = 1; }
            action ResolveRefusedPolicy when (phase == 31) {
                phase = 32; resolved = if Buggy == 18 {1} else {0};
            }
            action UnobservedHelper when (phase == 30) { phase = 33; policy = 1; }
            action ResolveUnobservedHelper when (phase == 33) {
                phase = 34; resolved = if Buggy == 17 {1} else {0};
            }
            action RebindSamePath when (phase == 4) { phase = 35; current_binding = 2; }
            action ExecuteReboundIdentity when (phase == 35) {
                phase = 36; executions = if Buggy == 19 {1} else {0};
            }
            invariant PreparationIsImmutable: intent == 1;
            invariant ExecutionNeedsResolution: executions == 0 || resolved == 1;
            invariant ResolutionIsImmutable: resolved <= 1;
            invariant RevocationPreventsExecution: revoked == 0 || executions == 0;
            invariant OriginalDeadlineBoundsExecution: expired == 0 || executions == 0;
            invariant ExecutionNeedsCurrentContext: context == 1 || executions == 0;
            invariant ExecuteOnce: executions <= 1;
            invariant ResultNeedsExactReceipt: result == 0 || (result == 1 && receipt == 1);
            invariant ResolutionNeedsExactChild: resolved == 0 || child == 1;
            invariant ResolutionNeedsObservedFacts: resolved == 0 || facts == 1;
            invariant ResolutionNeedsAdmittedPolicy: resolved == 0 || policy == 1;
            invariant ExecutionNeedsSelectedIdentity: executions == 0 || selected_binding == current_binding;
            invariant RevokedIntentCannotResolve: 20 > phase || phase > 21 || resolved == 0;
            invariant ExpiredIntentCannotResolve: 22 > phase || phase > 23 || resolved == 0;
            invariant ReplacedIntentCannotResolve: 24 > phase || phase > 25 || resolved == 0;
        }
    }
}

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
