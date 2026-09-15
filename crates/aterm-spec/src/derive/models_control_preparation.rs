// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Incoming control lanes must exist before adoption can be proved.

use super::Model;

#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_control_preparation_model() -> Model {
    crate::ty_model! {
        NativeUpdateControlPreparation {
            const Buggy = 0;
            var rpc = 0;
            var subscriptions = 0;
            var resolved = 0;
            var ready = 0;
            var proved = 0;

            action ReserveRpc when (resolved == 0 && rpc == 0) { rpc = 1; }
            action ReserveSubscription when (resolved == 0 && subscriptions == 0) {
                subscriptions = 1;
            }
            action Prepare when (
                resolved == 0 && ((rpc == 1 && subscriptions == 1) || Buggy == 1)
            ) {
                resolved = 1;
                ready = 1;
            }
            action Fail when (resolved == 0) { resolved = 1; }
            action Proof when (ready == 1 && proved == 0) { proved = 1; }

            invariant ReservedBeforeReady: ready == 0 || (rpc == 1 && subscriptions == 1);
            invariant ReservedBeforeProof: proved == 0 || (rpc == 1 && subscriptions == 1);
        }
    }
}
