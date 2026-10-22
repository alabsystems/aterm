// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pet locomotion admission under an observed but incoherent terminal surface.

use aterm_spec::derive::Model;

/// A missing observation and a failed observation carry different authority. A host that never
/// supplied a world keeps moving. While the host admits console presentation, a supplied but
/// incoherent world freezes existing locomotion and commissions no new motion; coherence restores
/// normal admission. Paths without console custody are not frozen by this rule; their host
/// withholds the caret instead.
///
/// The host admits presentation throughout; host-level suppression is a separate policy.
/// `legacy` projects `PetBrain::console_legacy_motion_pending`; `moved` records a changed body
/// position during the current tick, at fixed pane geometry. Motion outcomes are
/// nondeterministic: the trajectory, animation clock, protection policy and the timers/input
/// bookkeeping that continue while movement is suspended are out of the model. Tier-1 binds the
/// suspended transition to real Terminal/PetBrain frames. `Buggy=1` falls through into legacy
/// locomotion on a failed observation, catching both newly commissioned and active motion.
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
            // A coherence verdict is an observation's (dial 2 forgets the observation on a
            // tick and keeps its stale verdict).
            invariant CoherenceIsObserved: coherent <= observed;
            bound ObservedBounded: observed <= 1;
            bound LegacyBounded: legacy <= 1;
            bound PriorLegacyBounded: prior_legacy <= 1;
            bound MovedBounded: moved <= 1;
            bound TickedBounded: ticked <= 1;
        }
    }
}
