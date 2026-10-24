// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pet locomotion admission under an observed but incoherent terminal surface.

use aterm_spec::derive::Model;

/// A missing observation and a failed observation carry different authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn console_observation_admission_model() -> Model {
    aterm_spec::ty_model! {
        ConsoleObservationAdmission {
            const Buggy = 0;
            var observed = 0;
            var coherent = 0;
            var legacy = 0;
            var prior_legacy = 0;
            var moved = 0;
            var ticked = 0;

            action ObserveCoherent when (observed <= 1) {
                observed = 1; coherent = 1; moved = 0; ticked = 0;
            }
            action ObserveIncoherent when (observed <= 1) {
                observed = 1; coherent = 0; moved = 0; ticked = 0;
            }
            action TickStill when (legacy <= 1) {
                prior_legacy = legacy; moved = 0; ticked = 1;
                observed = if Buggy == 2 { 0 } else { observed };
            }
            action TickLaunch when (
                legacy == 0 && (observed == 0 || coherent == 1 || Buggy == 1)
            ) {
                prior_legacy = legacy; legacy = 1; moved = 0; ticked = 1;
            }
            action TickAdvance when (
                legacy == 1 && (observed == 0 || coherent == 1 || Buggy == 1)
            ) {
                prior_legacy = legacy; moved = 1; ticked = 1;
            }
            action TickRetire when (
                legacy == 1 && (observed == 0 || coherent == 1 || Buggy == 1)
            ) {
                prior_legacy = legacy; legacy = 0; moved = 0; ticked = 1;
            }

            invariant IncoherentObservationFreezesMotion:
                if ticked == 1 && observed == 1 && coherent == 0 {
                    legacy == prior_legacy && moved == 0
                } else { moved <= 1 };
            invariant CoherenceIsObserved: coherent <= observed;
            bound ObservedBounded: observed <= 1;
            bound LegacyBounded: legacy <= 1;
            bound PriorLegacyBounded: prior_legacy <= 1;
            bound MovedBounded: moved <= 1;
            bound TickedBounded: ticked <= 1;
        }
    }
}
