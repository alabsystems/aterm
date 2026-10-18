// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WARM SPARE SHELL's lifecycle (`docs/DESIGN-warm-spare-2026-10-16.md`
//! §6): one slot, its states, adoption and retirement. The real code is
//! `aterm-gui`'s `spare.rs` (`Spares`, `App::on_spare_spawned`,
//! `App::on_spare_note`, `App::spare_promote`, `App::try_adopt_spare`,
//! `App::adopt_live`, `App::retire_spare`); the Tier-1 bind is
//! `crates/aterm-gui/src/spare_conformance.rs`.

use aterm_spec::derive::Model;

/// The slot's `state`: 0 Absent, 1 Booked, 2 Spawned, 3 Parked, 4 Warming,
/// 5 Verifying, 6 Ready, 7 in transit (adopted, its landing not yet run).
///
/// `registered`: the current spare's id is in the store and the pool (it
/// landed); each booking reserves a fresh id, so a new spare starts unregistered.
/// `eligible`: the learned gate is open for the current rc generation; `stale`:
/// the generation moved since the spare's startup files ran; `exited`: its
/// shell is gone; `live`: adopted and live; `delivered`: host effects that
/// reached the host. `run_unproven` and `bad_adopt` latch a violation.
///
/// Invariants (design §6, I1–I6):
/// - I1 `NeverSlotAndRegistered`: a spare id is never both in the slot and in
///   the store or pool;
/// - I2 `RunOnlyWhenEligible`: `run` is written only while the gate is open;
/// - I3 `AdoptOnlyValid`: adoption only from Parked, Warming or Ready, never
///   stale, never exited;
/// - I4 `OneSpare`: at most one spare, slot and transit together;
/// - I5 `EveryExitAccounted`: a spare leaves the slot only by landing
///   (registered) or by retirement (killed and reaped), so the count of spares
///   always matches the slot;
/// - I6 `NoEffectBeforeAdoption`: no host effect is delivered before adoption.
///
/// `Buggy = 1` books a second spare beside a parked one, registers the spare as
/// it spawns, writes `run` while the gate is closed, adopts a stale or exited
/// spare, and delivers a host effect before adoption.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn spare_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        SpareLifecycle {
            const Buggy = 0;
            const MaxEffects = 1;
            var state = 0;
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
                spares = spares + 1;
                registered = 0;
                live = 0;
                stale = 0;
                exited = 0;
            }
            action Spawned when (state == 1) {
                registered = if Buggy == 1 { 1 } else { registered };
                state = 2;
            }
            action ParkMark when (state == 2) {
                state = 3;
            }
            action GateOpens when (eligible == 0) {
                eligible = 1;
            }
            action GenerationMoves when (eligible == 1) {
                eligible = 0;
                stale = if state > 3 && state <= 7 { 1 } else { stale };
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
            action Adopt when ((state == 3 || state == 4 || state == 6)
                && (Buggy == 1 || stale == 0 && exited == 0 && (state == 3 || eligible == 1))) {
                bad_adopt = if stale == 1 || exited == 1 { 1 } else { bad_adopt };
                state = 7;
            }
            action Land when (state == 7) {
                state = 0;
                spares = spares - 1;
                registered = 1;
                live = 1;
            }
            action Retire when (state > 0 && state <= 6) {
                state = 0;
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
