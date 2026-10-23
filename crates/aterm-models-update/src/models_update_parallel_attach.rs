// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model for successor layout rebuild scheduling (`restore_pass_due`) and adoption proof.

use aterm_spec::derive::{Liveness, Model, and_, eq, int, or_, var};

/// Returns the TLA+ model for successor layout rebuild and adoption proof.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_successor_attach_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateSuccessorAttach {
            const Buggy = 0;
            var lane = 0;
            var many = 0;
            var phase = 0;
            var queued = 1;
            var extras = 0;
            var kicked = 0;
            var painted0 = 0;
            var painted_extras = 0;
            var proved = 0;
            var handed = 0;
            var draining = 0;
            action Widen when (lane == 0 && many == 0) {
                many = 1;
            }
            action StartHandoff when (lane == 0) {
                lane = 1;
                handed = 1;
            }
            action StartCold when (lane == 0) {
                lane = 2;
            }
            action StartDegraded when (lane == 0) {
                lane = 2;
                handed = 1;
            }
            action Attach0 when (lane > 0 && phase == 0) {
                phase = 1;
            }
            action RebuildAtAttach when (phase == 1 && queued == 1 && lane == 1) {
                queued = 0;
                extras = many;
                kicked = many;
            }
            action LeaveResume when (phase == 1 && (queued == 0 || lane == 2)) {
                phase = 2;
            }
            action RebuildAtPark when (
                phase == 2 && queued == 1 && painted0 + painted_extras > 0
            ) {
                queued = 0;
                extras = many;
                kicked = many;
                draining = 0;
            }
            action RebuildSliceAtPark when (
                phase == 2 && queued == 1 && painted0 + painted_extras > 0 && handed == 0
            ) {
                draining = 1;
            }
            action Present0 when (phase > 0 && painted0 == 0) {
                painted0 = 1;
            }
            action PresentExtras when (extras == 1 && kicked == 1 && painted_extras == 0) {
                painted_extras = 1;
            }
            action Prove when (
                lane == 1 && proved == 0 && painted0 == 1 &&
                queued == 0 && painted_extras == extras
            ) {
                proved = 1;
            }
            action RebuildAtAttachCold when (
                Buggy == 1 && phase == 1 && queued == 1 && lane == 2
            ) {
                queued = 0;
                extras = many;
                kicked = many;
            }
            action LeaveResumeOwing when (
                Buggy == 1 && phase == 1 && queued == 1 && lane == 1
            ) {
                phase = 2;
            }
            action ProveAtFirstPresent when (
                Buggy == 1 && lane == 1 && proved == 0 && painted0 == 1
            ) {
                proved = 1;
            }
            action RebuildSliceHanded when (
                Buggy == 1 && phase == 2 && queued == 1 &&
                painted0 + painted_extras > 0 && handed == 1
            ) {
                draining = 1;
            }
            action RebuildSliceBeforePaint when (
                Buggy == 1 && phase == 1 && queued == 1 && handed == 0 &&
                painted0 + painted_extras == 0
            ) {
                draining = 1;
            }
            action RebuildWithoutKick when (
                Buggy == 1 && phase == 1 && queued == 1 && lane == 1 && many == 1
            ) {
                queued = 0;
                extras = 1;
                kicked = 0;
            }
            invariant ColdRestoreAfterFirstPaint:
                lane == 1 || queued == 1 || painted0 + painted_extras > 0;
            invariant HandoffAttachesInOnePass:
                lane == 2 || phase <= 1 || queued == 0;
            invariant EveryRebuiltWindowPresents: kicked == extras;
            invariant ProofNeedsDrainedAndPainted:
                proved == 0 || (queued == 0 && painted0 == 1 && painted_extras == extras);
            invariant HandedRebuildsWhole: handed == 0 || draining == 0;
            invariant ColdSlicesAfterFirstPaint:
                draining == 0 || painted0 + painted_extras > 0;
        }
    }
}

/// Progress property for [`native_update_successor_attach_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(native_update_successor_attach_model)]
pub fn native_update_successor_attach_liveness() -> Liveness {
    Liveness {
        name: "TheSuccessorProves",
        goal: or_(
            eq(var("proved"), int(1)),
            and_(
                eq(var("lane"), int(2)),
                and_(
                    eq(var("queued"), int(0)),
                    and_(
                        eq(var("painted0"), int(1)),
                        eq(var("painted_extras"), var("extras")),
                    ),
                ),
            ),
        ),
        weak: vec![
            "StartHandoff",
            "Attach0",
            "RebuildAtAttach",
            "LeaveResume",
            "RebuildAtPark",
            "Present0",
            "PresentExtras",
            "Prove",
        ],
        strong: vec![],
        mutants: vec!["RebuildWithoutKick"],
    }
}
