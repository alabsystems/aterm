// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Passive Relaunch evidence survives the built-in's own claim lifecycle, while a foreign
/// takeover revokes its old epoch permanently.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_observation_continuity_model() -> Model {
    aterm_spec::ty_model! {
        NativeObservationContinuity {
            const Buggy = 0;
            var phase = 0; var revision = 0; var epoch = 0; var captured = 0;
            var builtin = 1; var proof = 0; var exhausted = 0;
            action Capture when (phase == 0) { phase = 1; captured = epoch; }
            action OwnClaim when (phase == 1) {
                phase = 2; revision = 1; epoch = if Buggy == 1 {1} else {epoch};
            }
            action OwnRenew when (phase == 2) {
                phase = 15; revision = revision + 1; epoch = if Buggy == 8 {epoch + 1} else {epoch};
            }
            action OwnRelease when (phase == 2 || phase == 15) {
                phase = 3; revision = revision + 1; epoch = if Buggy == 2 {epoch + 1} else {epoch};
            }
            action OwnRetake when (phase == 3) { phase = 4; revision = revision + 1; }
            action ProveOwnContinuity when (phase == 4) {
                phase = 5; proof = if captured == epoch {1} else {0};
            }
            action ForeignRelaunch when (phase == 1) {
                phase = 7; revision = 1; builtin = 0;
                epoch = if Buggy == 3 {epoch} else {epoch + 1};
            }
            action ForeignReturn when (phase == 7) {
                phase = 8; revision = 2; builtin = 1; epoch = epoch + 1;
            }
            action ProveAfterForeignReturn when (phase == 8) {
                phase = 9; proof = if Buggy == 4 {1} else {if captured == epoch {1} else {0}};
            }
            action ForeignExpiry when (phase == 7) { phase = 10; builtin = 1; }
            action ProveAfterForeignExpiry when (phase == 10) {
                phase = 11; proof = if captured == epoch {1} else {0};
            }
            action ForeignUpgradeOnly when (phase == 1) {
                phase = 12; revision = 1; epoch = if Buggy == 5 {epoch + 1} else {epoch};
            }
            action ProveUnrelatedDuty when (phase == 12) {
                phase = 13; proof = if captured == epoch {1} else {0};
            }
            // A boundary fixture compresses u64::MAX - 1 and MAX to 1 and 2;
            // the captured old zero remains distinct until a broken counter wraps.
            action NearExhaustion when (phase == 1) { phase = 16; epoch = 1; }
            action OverflowForeign when (phase == 16) {
                phase = 17; revision = 1; epoch = 2; builtin = 0;
            }
            action OverflowReturn when (phase == 17) {
                phase = 18; revision = 2;
                epoch = if Buggy == 6 {0} else {2};
                exhausted = if Buggy == 6 {0} else {1};
                builtin = if Buggy == 6 {1} else {0};
            }
            action ProveAfterOverflow when (phase == 18) {
                phase = 19; proof = if exhausted == 0 && captured == epoch {1} else {0};
            }
            action CaptureAfterOverflow when (phase == 18) {
                phase = 20; proof = if Buggy == 7 {1} else {builtin};
            }
            invariant OwnLifecycleAvailable: 5 > phase || phase > 5 || proof == 1;
            invariant ForeignReturnNeverRevives: 9 > phase || phase > 9 || proof == 0;
            invariant ForeignExpiryNeverRevives: 11 > phase || phase > 11 || proof == 0;
            invariant OtherDutyDoesNotRevoke: 13 > phase || phase > 13 || proof == 1;
            invariant ExhaustionNeverRevives: 19 > phase || phase > 19 || proof == 0;
            invariant ExhaustionCannotCapture: 20 > phase || phase > 20 || proof == 0;
        }
    }
}
