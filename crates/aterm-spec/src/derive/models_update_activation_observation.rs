// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An inconclusive installed-bundle probe cannot retire an activation intent.

use super::Model;

/// The service has already imported a verified activation. An observation can
/// confirm its identity, contradict it, or fail to establish either. Both an
/// idle reconcile and a returned apply retain the stage until contradicted.
/// `Buggy=1` reproduces retiring on a failed probe in the returned-apply lane.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_activation_observation_model() -> Model {
    crate::ty_model! {
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
                retained = if changed == 1 || (Buggy == 1 && returned == 1 && observed == 0) {
                    0
                } else {
                    1
                };
                reduced = 1;
            }

            invariant NoRetirementWithoutEvidence: retained == 1 || changed == 1;
            invariant ChangedIdentityRetires: reduced == 0 || changed == 0 || retained == 0;
        }
    }
}
