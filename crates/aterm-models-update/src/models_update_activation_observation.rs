// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An inconclusive installed-bundle probe cannot retire an activation intent.

use aterm_spec::derive::Model;

/// Model verifying that an imported activation stage is retained until an observation contradicts its identity.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_activation_observation_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateActivationObservation {
            const Buggy = 0;
            var observed = 0;
            var changed = 0;
            var returned = 0;
            var reduced = 0;
            var retained = 1;

            action ObserveMatch when (reduced == 0 && observed == 0) {
                observed = 1;
            }
            action ObserveChange when (reduced == 0 && observed == 0) {
                observed = 1;
                changed = 1;
            }
            action Return when (reduced == 0 && returned == 0) {
                returned = 1;
            }
            action Reduce when (reduced == 0) {
                retained = if Buggy == 2 {
                    1
                } else {
                    if changed == 1 || (Buggy == 1 && returned == 1 && observed == 0) {
                        0
                    } else {
                        1
                    }
                };
                reduced = 1;
            }

            invariant NoRetirementWithoutEvidence: retained == 1 || changed == 1;
            invariant ChangedIdentityRetires: reduced == 0 || changed == 0 || retained == 0;
        }
    }
}
