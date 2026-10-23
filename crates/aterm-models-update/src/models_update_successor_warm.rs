// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The successor's boot around its claim.

use aterm_spec::derive::*;

/// `NativeUpdateSuccessorWarmBeforeClaim`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_successor_warm_before_claim_model() -> Model {
    aterm_spec::ty_model! {
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
            var relative_socket = 0;
            var warm_done = 0;
            var socket_dir_ready = 0;
            var cwd_mut_after_spawn = 0;
            var build_size_ok = 1;
            var config_current = 1;

            action RelativeSocket when (phase == 0 && relative_socket == 0) {
                relative_socket = 1;
            }
            action Capture when (phase == 0) {
                captured = 1;
                env_mut_after_spawn = threads_spawned;
                phase = 1;
            }
            action SpawnWarm when (phase == 1 && threads_spawned == 0 && relative_socket == 0) {
                threads_spawned = 1;
            }
            action WarmDone when (phase == 1 && threads_spawned == 1 && warm_done == 0) {
                warm_done = 1;
            }
            action WarmPresent when (phase == 1 && warm_presented == 0) {
                warm_presented = 1;
            }
            action StaleHint when (phase == 1 && relative_socket == 0 && build_size_ok == 1) {
                build_size_ok = 0;
            }
            action ConfigEdit when (
                phase > 0 && phase <= 2 && threads_spawned == 1 && config_current == 1
            ) {
                config_current = 0;
            }
            action Dial when (
                phase == 1 && captured == 1 && (warm_done == 1 || relative_socket == 1)
            ) {
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
            action PrepareSocketDir when (
                phase == 4 && relative_socket == 1 && socket_dir_ready == 0
            ) {
                socket_dir_ready = 1;
                cwd_mut_after_spawn = threads_spawned;
            }
            action SpawnLate when (
                phase == 4 && relative_socket == 1 && socket_dir_ready == 1 && threads_spawned == 0
            ) {
                threads_spawned = 1;
                warm_done = 1;
            }
            action Reshape when (phase == 4 && build_size_ok == 0) {
                build_size_ok = 1;
            }
            action Rewarm when (phase == 4 && config_current == 0) {
                config_current = 1;
                build_size_ok = 1;
            }
            action RealPresent when (
                phase == 4 && carried_presented == 0 && warm_done == 1 && build_size_ok == 1
                    && config_current == 1
            ) {
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
            action WarmBeforeDialOnRelativeSocket when (
                Buggy == 1 && phase == 1 && relative_socket == 1 && threads_spawned == 0
            ) {
                threads_spawned = 1;
                warm_done = 1;
            }
            action PresentAtHintedSize when (
                Buggy == 1 && phase == 4 && carried_presented == 0 && warm_done == 1
                    && build_size_ok == 0
            ) {
                carried_presented = 1;
            }
            action PresentUnderStaleConfig when (
                Buggy == 1 && phase == 4 && carried_presented == 0 && warm_done == 1
                    && build_size_ok == 1 && config_current == 0
            ) {
                carried_presented = 1;
            }
            action JoinWorkerBeforeDial when (
                Buggy == 1 && phase == 1 && threads_spawned == 1 && warm_done == 0
            ) {
                warm_done = 2;
            }

            invariant NoEnvMutationAfterSpawn: env_mut_after_spawn == 0;
            invariant NothingOwnedBeforeClaim: owned_taken == 0 || claim_ok == 1;
            invariant NeverRevealedBeforeClaim:
                (window_visible == 0 && carried_presented == 0) || claim_ok == 1;
            invariant WarmPresentNeverProves: phase <= 4 || phase == 7 || carried_presented == 1;
            invariant HintNotInProof: hint_in_digest == 0;
            invariant FailedClaimLeavesNothing:
                phase <= 6 || claim_ok == 1 || (owned_taken == 0 && window_visible == 0);
            invariant NoCwdChangeAfterSpawn: cwd_mut_after_spawn == 0;
            invariant CarriedPresentAtCarriedSize: carried_presented == 0 || build_size_ok == 1;
            invariant CarriedPresentUnderCurrentConfig:
                carried_presented == 0 || config_current == 1;
            invariant PrologueJoinsNoThread: warm_done <= 1;
        }
    }
}

/// [`native_update_successor_warm_before_claim_model`]'s progress claim.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(native_update_successor_warm_before_claim_model)]
pub fn native_update_successor_warm_before_claim_liveness() -> Liveness {
    Liveness {
        name: "TheWarmSuccessorProvesOrExits",
        goal: gt(var("phase"), int(4)),
        weak: vec![
            "Capture",
            "SpawnWarm",
            "WarmDone",
            "Dial",
            "ClaimFail",
            "TakeOwned",
            "Adopt",
            "PrepareSocketDir",
            "SpawnLate",
            "Reshape",
            "Rewarm",
            "RealPresent",
            "Prove",
        ],
        strong: vec![],
        mutants: vec!["JoinWorkerBeforeDial"],
    }
}
