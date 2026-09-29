// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The successor's boot around its claim: what it may do before the outgoing
//! process has handed anything over (docs/DESIGN-warm-successor-2026-09-29.md,
//! "The derived model and invariants").

use super::*;

/// `NativeUpdateSuccessorWarmBeforeClaim`: a successor may WARM before its
/// claim — spawn threads, present into hidden windows — but may not mutate the
/// process environment once a thread exists, own anything the outgoing process
/// still owns, show anything, or let a warm present prove.
///
/// `phase`: 0 Launched, 1 Warming (the prologue after the handoff snapshot and
/// before the dial), 2 Dialled, 3 Claimed, 4 Adopted, 5 Proved, 6 Committed,
/// 7 Exited.
///
/// THE P1 HALF (`HandoffEnv`, aterm-gui's `handoff_env.rs`) is what makes the
/// first invariant hold by construction: `Capture` is the one environment
/// mutation, it happens in `Launched` before any `SpawnWarm`, and the claimed
/// descriptors land in the snapshot rather than in `environ`.
/// `PublishToEnv` (dead at `Buggy = 0`) is `ClaimedHandoff::publish`, which
/// wrote them into the process environment after the claim: harmless while no
/// thread could exist before the claim, a data race the moment one does (P2).
///
/// Each other `Buggy = 1` mutant is its own action, so none masks another:
/// `BindWhileWarming` takes an owned resource (the control socket) before the
/// claim; `RevealPlaceholder` shows a warm window over the frozen parent;
/// `WarmProves` lets a warm present stand for the carried screens; `FoldHint`
/// folds the advisory launch hint into the digests. Tier-1 (aterm-gui's
/// `handoff_env::conformance`) drives the real snapshot and intake; the warm
/// actions have no code until P2–P4 and are waived there by name.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_successor_warm_before_claim_model() -> Model {
    crate::ty_model! {
        NativeUpdateSuccessorWarmBeforeClaim {
            const Buggy = 0;
            var phase = 0;
            var captured = 0;
            var threads_spawned = 0;
            var env_mut_after_spawn = 0;
            var owned_taken = 0;
            var window_visible = 0;
            var warm_presented = 0;
            var carried_presented = 0;
            var claim_ok = 0;
            var hint_in_digest = 0;

            // The one environment mutation: read and clear every handoff name,
            // while no thread exists.
            action Capture when (phase == 0) {
                captured = 1;
                env_mut_after_spawn = threads_spawned;
                phase = 1;
            }
            action SpawnWarm when (phase == 1 && threads_spawned == 0) {
                threads_spawned = 1;
            }
            action WarmPresent when (phase == 1 && warm_presented == 0) {
                warm_presented = 1;
            }
            action Dial when (phase == 1 && captured == 1) {
                phase = 2;
            }
            action ClaimOk when (phase == 2) {
                phase = 3;
                claim_ok = 1;
            }
            action ClaimFail when (phase == 2) {
                phase = 7;
            }
            action TakeOwned when (phase == 3 && claim_ok == 1 && owned_taken == 0) {
                owned_taken = 1;
            }
            action Adopt when (phase == 3 && owned_taken == 1) {
                phase = 4;
            }
            action RealPresent when (phase == 4 && carried_presented == 0) {
                carried_presented = 1;
            }
            action Reveal when (claim_ok == 1 && carried_presented == 1 && window_visible == 0) {
                window_visible = 1;
            }
            action Prove when (phase == 4 && carried_presented == 1) {
                phase = 5;
            }
            action Commit when (phase == 5) {
                phase = 6;
            }

            // Mutants, dead at Buggy = 0.
            action PublishToEnv when (Buggy == 1 && phase == 3 && env_mut_after_spawn == 0) {
                env_mut_after_spawn = threads_spawned;
            }
            action BindWhileWarming when (Buggy == 1 && phase == 1 && owned_taken == 0) {
                owned_taken = 1;
            }
            action RevealPlaceholder when (Buggy == 1 && phase == 1 && window_visible == 0) {
                window_visible = 1;
            }
            action WarmProves when (
                Buggy == 1 && phase == 4 && warm_presented == 1 && carried_presented == 0
            ) {
                phase = 5;
            }
            action FoldHint when (Buggy == 1 && phase == 4 && hint_in_digest == 0) {
                hint_in_digest = 1;
            }

            invariant NoEnvMutationAfterSpawn: env_mut_after_spawn == 0;
            invariant NothingOwnedBeforeClaim: owned_taken == 0 || claim_ok == 1;
            invariant NeverRevealedBeforeClaim:
                (window_visible == 0 && carried_presented == 0) || claim_ok == 1;
            invariant WarmPresentNeverProves: phase <= 4 || phase == 7 || carried_presented == 1;
            invariant HintNotInProof: hint_in_digest == 0;
            invariant FailedClaimLeavesNothing:
                phase <= 6 || claim_ok == 1 || (owned_taken == 0 && window_visible == 0);
        }
    }
}
