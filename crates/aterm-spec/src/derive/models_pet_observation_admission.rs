// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pet locomotion admission under an observed but incoherent terminal surface.

use super::Model;

/// A missing observation and a failed observation carry different authority.
/// Legacy hosts that never supplied a world can keep moving. While the host
/// admits console presentation, a supplied but incoherent world must freeze
/// existing locomotion and must not commission new motion. Coherence recovery
/// restores normal admission. Paths without console custody are not frozen by
/// this rule; their host withholds the caret instead.
///
/// The host has admitted presentation throughout this model; host-level
/// suppression is a separate policy. `legacy` projects
/// `PetBrain::console_legacy_motion_pending`; `moved` records a changed body
/// position during the current tick, at FIXED pane geometry.
/// These are nondeterministic motion outcomes, not a model of the trajectory,
/// animation clock, protection policy, or the timers/input bookkeeping which
/// must continue while movement is suspended. Observation alone moves nothing.
/// Tier-1 binds the suspended transition to real Terminal/PetBrain frames.
/// `Buggy=1` restores the old fallthrough into legacy locomotion on a failed
/// observation, catching both newly commissioned and already active motion.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn console_observation_admission_model() -> Model {
    crate::ty_model! {
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
            invariant StateBounded:
                observed <= 1 && coherent <= observed && legacy <= 1
                    && prior_legacy <= 1 && moved <= 1 && ticked <= 1;
        }
    }
}
