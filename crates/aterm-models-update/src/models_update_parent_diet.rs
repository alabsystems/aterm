// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the speculative pre-park layout sidecar (`NativeUpdateSpeculativeSidecar`).

use aterm_spec::derive::{Liveness, Model, gt, int, var};

/// Returns the `NativeUpdateSpeculativeSidecar` safety model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_speculative_sidecar_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateSpeculativeSidecar {
            const Buggy = 0;
            var phase = 0;
            var fails = 0;
            var spec = 0;
            var moved = 0;
            var disk = 0;
            var verified = 0;
            var read = 0;
            var awaiting = 0;

            action RefuseWrite when (phase == 0 && fails == 0) {
                fails = 1;
            }
            action Speculate when (phase == 0) {
                spec = if (fails == 1) { 0 } else { 1 };
                disk = if (fails == 1) { 0 } else { 1 };
                phase = 1;
            }
            action LayoutMoves when (phase == 1 && moved == 0) {
                moved = 1;
            }
            action DiskDisturbed when (phase == 1 && disk == 1) {
                disk = 3;
            }
            action Park when (phase == 1) {
                phase = 2;
            }
            action KeepVerified when (phase == 2 && spec == 1 && disk == 1 && moved == 0) {
                verified = 1;
                phase = 3;
            }
            action DiscardAndRewrite when (phase == 2 && spec == 1 && (moved == 1 || disk == 3)) {
                disk = 2;
                phase = 3;
            }
            action WriteAtPark when (phase == 2 && spec == 0 && awaiting == 0) {
                disk = 2;
                phase = 3;
            }
            action Grant when (phase == 3) {
                phase = 4;
            }
            action SuccessorReads when (phase == 4 && read == 0) {
                read = if (disk == 2 || (disk == 1 && moved == 0)) { 1 } else { 2 };
                disk = 0;
            }
            action Retire when (phase > 0 && phase <= 4) {
                disk = 0;
                phase = 5;
            }

            action SealUnverified when (Buggy == 1 && phase == 2 && spec == 1 && disk == 1) {
                phase = 3;
            }
            action VerifyTheMemoryCopy when (
                Buggy == 1 && phase == 2 && spec == 1 && moved == 0 && disk == 3
            ) {
                verified = 1;
                phase = 3;
            }
            action GrantBeforeSeal when (Buggy == 1 && phase == 2 && spec == 1) {
                phase = 4;
            }
            action RetireKeepsSpeculation when (
                Buggy == 1 && phase > 0 && phase <= 4 && disk == 1
            ) {
                phase = 5;
            }
            action AwaitSpeculation when (
                Buggy == 1 && phase == 2 && spec == 0 && awaiting == 0
            ) {
                awaiting = 1;
            }

            invariant SpeculationVerifiedBeforeSeal:
                phase <= 2 || phase > 3 || disk <= 0 || disk > 1 || verified == 1;
            invariant SealHoldsTheParksBytes:
                phase <= 2 || phase > 3 || disk == 2 || (disk == 1 && moved == 0);
            invariant SuccessorReadsTheParksBytes: read <= 1;
            invariant NothingSpeculativeOutlivesTheAttempt: phase <= 4 || disk == 0;
            invariant ParkNeverWaitsOnSpeculation: awaiting == 0;
        }
    }
}

/// Returns the liveness specification for [`native_update_speculative_sidecar_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(native_update_speculative_sidecar_model)]
pub fn native_update_speculative_sidecar_liveness() -> Liveness {
    Liveness {
        name: "TheAttemptIsGrantedOrRetired",
        goal: gt(var("phase"), int(3)),
        weak: vec![
            "Speculate",
            "Park",
            "KeepVerified",
            "DiscardAndRewrite",
            "WriteAtPark",
            "Grant",
        ],
        strong: vec![],
        mutants: vec!["AwaitSpeculation"],
    }
}
