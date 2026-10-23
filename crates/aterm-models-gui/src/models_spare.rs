// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model for the warm spare shell lifecycle.

use aterm_spec::derive::Model;

/// Returns the formal model for the warm spare shell lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn spare_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        SpareLifecycle {
            const Buggy = 0;
            const MaxEffects = 1;
            var state = 0;
            var warm = 0;
            var spares = 0;
            var registered = 0;
            var eligible = 0;
            var stale = 0;
            var exited = 0;
            var live = 0;
            var delivered = 0;
            var run_unproven = 0;
            var bad_adopt = 0;

            action Book when (state == 0 || Buggy == 1 && state == 3) {
                state = 1;
                warm = 0;
                spares = spares + 1;
                registered = 0;
                live = 0;
                stale = 0;
                exited = 0;
            }
            action BookWarm when (state == 0 && (eligible == 1 || Buggy == 1)) {
                run_unproven = if eligible == 0 { 1 } else { run_unproven };
                state = 1;
                warm = 1;
                spares = spares + 1;
                registered = 0;
                live = 0;
                stale = 0;
                exited = 0;
            }
            action Spawned when (state == 1 && warm == 0) {
                registered = if Buggy == 1 { 1 } else { registered };
                state = 2;
            }
            action SpawnedWarm when (state == 1 && warm == 1) {
                registered = if Buggy == 1 { 1 } else { registered };
                state = 4;
            }
            action ParkMark when (state == 2) {
                state = 3;
            }
            action GateOpens when (eligible == 0) {
                eligible = 1;
            }
            action GenerationMoves when (eligible == 1) {
                eligible = 0;
                stale = if (state > 3 && state <= 7) || (state == 1 && warm == 1) { 1 } else { stale };
            }
            action Promote when (state == 3 && (eligible == 1 || Buggy == 1)) {
                run_unproven = if eligible == 0 { 1 } else { run_unproven };
                state = 4;
            }
            action StartMark when (state == 4) {
                state = 5;
            }
            action Verified when (state == 5 && stale == 0 && exited == 0) {
                state = 6;
            }
            action Exits when (state > 1 && state <= 6 && exited == 0) {
                exited = 1;
            }
            action Adopt when ((state == 3 || state == 4 || state == 5 || state == 6)
                && (Buggy == 1 || stale == 0 && exited == 0 && (state == 3 || eligible == 1))) {
                bad_adopt = if stale == 1 || exited == 1 { 1 } else { bad_adopt };
                state = 7;
                warm = 0;
            }
            action Land when (state == 7) {
                state = 0;
                spares = spares - 1;
                registered = 1;
                live = 1;
            }
            action Retire when (state > 0 && state <= 6) {
                state = 0;
                warm = 0;
                spares = spares - 1;
                stale = 0;
                exited = 0;
            }
            action HostEffect when (state > 1 && state <= 6 && delivered <= MaxEffects - 1) {
                delivered = if Buggy == 1 { delivered + 1 } else { delivered };
            }

            invariant NeverSlotAndRegistered: state == 0 || state == 7 || registered == 0;
            invariant RunOnlyWhenEligible: run_unproven == 0;
            invariant AdoptOnlyValid: bad_adopt == 0;
            invariant OneSpare: spares <= 1;
            invariant EveryExitAccounted: (state == 0 && spares == 0) || (state > 0 && spares == 1);
            invariant NoEffectBeforeAdoption: delivered == 0 || live == 1;
        }
    }
}
