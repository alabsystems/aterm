// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A queued batch may lose authority before its first write. An admitted batch instead owns
/// all its bytes until completion; a transition drains its captured revisions, not newer work.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn scoped_input_drain_model() -> Model {
    aterm_spec::ty_model! {
        ScopedInputDrain {
            const Buggy = 0;
            var phase = 0; var revision = 0; var old = 0; var fresh = 0;
            var started = 0; var stale_byte = 0; var conflict = 0; var tail = 0; var ack = 0;
            action Queue when (phase == 0) { phase = 1; }
            action Start when (phase == 1) { phase = 2; old = 1; started = 1; }
            action TransitionQueued when (phase == 1) { phase = 3; revision = 1; }
            action TransitionWriting when (phase == 2) { phase = 3; revision = 1; }
            action Stale when (phase == 3) {
                phase = 4; stale_byte = if Buggy == 1 {1} else {0};
            }
            action ConflictingStart when (phase == 4) {
                phase = 5; conflict = if old == 0 || Buggy == 2 {1} else {0};
            }
            action NewWork when (phase == 5) { phase = 6; fresh = 1; }
            action Continue when (phase == 6) {
                phase = 7; tail = if Buggy == 3 {0} else {started};
            }
            action EarlyAck when (phase == 7) {
                phase = 8; ack = if old == 0 || Buggy == 4 {1} else {0};
            }
            action Finish when (phase == 8) { phase = 9; old = 0; }
            action Ack when (phase == 9) {
                phase = 10; ack = if Buggy == 5 && fresh == 1 {0} else {1};
            }
            invariant StaleQueueWritesNothing: stale_byte == 0;
            invariant OldWriterExcludesConflictingActor: old == 0 || conflict == 0;
            invariant StartedCommandFinishes: 7 > phase || tail == started;
            invariant AckWaitsForOldWriter: ack == 0 || old == 0;
            invariant NewWorkDoesNotStarveDrain: 10 > phase || ack == 1;
        }
    }
}

/// Sealing stops the producer, but queued actual work still owns the lease. Once that work
/// finishes, response readers may retain all their observation handles without holding it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn scoped_input_seal_model() -> Model {
    aterm_spec::ty_model! {
        ScopedInputSeal {
            const Buggy = 0;
            var phase = 0; var sealed = 0; var pending = 0; var lease = 0; var ack = 0;
            action Start when (phase == 0) { phase = 1; pending = 1; lease = 1; }
            action SealPending when (phase == 1) {
                phase = 3; sealed = 1; lease = if Buggy == 1 {0} else {1};
            }
            action FinishUnsealed when (phase == 1) {
                phase = 2; pending = 0; lease = if Buggy == 3 {0} else {1};
            }
            action SealDone when (phase == 2) {
                phase = 4; sealed = 1; lease = if Buggy == 2 {1} else {0};
            }
            action FinishPending when (phase == 3) {
                phase = 4; pending = 0; lease = if Buggy == 2 {1} else {0};
            }
            action ReadWait when (phase == 4) { phase = 5; }
            action Ack when (phase == 5) { phase = 6; ack = if lease == 0 {1} else {0}; }
            invariant PendingWorkOwnsLease: pending == 0 || lease == 1;
            invariant ProducerCanFinishItsSuffix: 2 > phase || phase > 2 || lease == 1;
            invariant ResponseReadersOwnNoLease: 4 > phase || lease == 0;
            invariant AckDoesNotWaitForResponse: 6 > phase || ack == 1;
        }
    }
}
