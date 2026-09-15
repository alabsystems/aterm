// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An artifact-scoped retry latch ends when its own stage is retired.

use super::Model;

/// Reconciliation retires the stage whose durable identity changed. It releases
/// only a latch naming those exact bytes; a later artifact's latch is unrelated.
/// `Buggy=1` reproduces leaving a withdrawn build's permanent latch behind.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_retired_intent_model() -> Model {
    crate::ty_model! {
        NativeUpdateRetiredIntent {
            const Buggy = 0;
            var retired = 0;
            var exact = 1;
            var latched = 1;

            action OtherArtifact when (retired == 0 && exact == 1) {
                exact = 0;
            }
            action Retire when (retired == 0) {
                retired = 1;
                latched = if exact == 1 && Buggy == 0 { 0 } else { 1 };
            }

            invariant NoObsoleteLatch: retired == 0 || exact == 0 || latched == 0;
            invariant OtherArtifactUntouched: exact == 1 || latched == 1;
        }
    }
}

/// A failed attempt remains attributed to its original artifact after a newer
/// stage is imported. `Buggy=1` restores deriving the target at publication time.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_failure_target_model() -> Model {
    crate::ty_model! {
        NativeUpdateFailureTarget {
            const Buggy = 0;
            var staged = 1;
            var attempted = 1;
            var charged = 0;

            action ReplaceStage when (charged == 0 && staged == 1) {
                staged = 2;
            }
            action Publish when (charged == 0) {
                charged = if Buggy == 1 { staged } else { attempted };
            }

            invariant FailureBelongsToAttempt: charged == 0 || charged == attempted;
        }
    }
}
