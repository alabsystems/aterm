// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AUTOPILOT'S TIMER: Cruise until a time, then Land.

use aterm_spec::derive::{Liveness, Model, gt, int, or_, var};

/// The timer's world, bounded: a wall clock of `MaxWall` minutes, an arm `Lead` minutes ahead.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn autopilot_timer_model() -> Model {
    aterm_spec::ty_model! {
        AutopilotTimer {
            const Buggy = 0;
            const MaxWall = 3;
            const Lead = 2;
            var word = 0;
            var land_at = 0;
            var idx = 0;
            var wait = 0;
            var wake = 0;
            var wall = 0;
            var committed = 1;
            var alive = 1;
            var bit = 0;
            var leaf = 0;
            var leaf_word = 0;
            var leaf_land = 0;
            var hand = 0;
            var fires = 0;
            var over_hand = 0;
            var early = 0;

            action SetCruiseUntil when (alive == 1 && committed == 1 && wall <= MaxWall - Lead) {
                word = 1;
                land_at = wall + Lead;
                idx = wall + Lead;
                wait = Lead;
                bit = 1;
                hand = 0;
                fires = 0;
            }
            action WireStoreUntil when (alive == 1 && wall <= MaxWall - Lead) {
                word = 1;
                land_at = wall + Lead;
                wake = if (word == 1 && land_at == wall + Lead) { wake } else { 1 };
                bit = 1;
                hand = 0;
                fires = 0;
            }
            action WireCruise when (alive == 1) {
                word = 1;
                land_at = 0;
                wake = if (word == 1 && land_at == 0) { wake } else { 1 };
                bit = 1;
                hand = 1;
            }
            action WireLand when (alive == 1) {
                word = 2;
                land_at = 0;
                wake = if (word == 2 && land_at == 0) { wake } else { 1 };
                bit = 1;
                hand = 1;
            }
            action WireOff when (alive == 1) {
                word = 3;
                land_at = 0;
                wake = if (word == 3 && land_at == 0) { wake } else { 1 };
                bit = 1;
                hand = 1;
            }
            action Unset when (alive == 1) {
                word = 0;
                land_at = 0;
                wake = if (word == 0 && land_at == 0) { wake } else { 1 };
                bit = 1;
                hand = 1;
            }
            action ApplyWake when (alive == 1 && wake == 1) {
                idx = if (committed == 1) { land_at } else { idx };
                wait = if (committed == 1) {
                    if (land_at > wall) { land_at - wall } else { 0 }
                } else {
                    wait
                };
                wake = 0;
            }
            action Tick when (wall <= MaxWall - 1) {
                wall = wall + 1;
                wait = if (wait > 0) { wait - 1 } else { 0 };
            }
            action Sleep when (wall <= MaxWall - 1) {
                wall = wall + 1;
            }
            action ClockBack when (wall > 0) {
                wall = wall - 1;
            }
            action Fire when (alive == 1 && committed == 1 && idx > 0 && wait == 0) {
                word = if (word == 1 && land_at == idx && idx <= wall) { 2 } else { word };
                land_at = if (word == 1 && land_at == idx && idx <= wall) { 0 } else { land_at };
                bit = if (word == 1 && land_at == idx && idx <= wall) { 1 } else { bit };
                fires = if (word == 1 && land_at == idx && idx <= wall) {
                    if (fires > 0) { 2 } else { 1 }
                } else {
                    fires
                };
                over_hand = if (word == 1 && land_at == idx && idx <= wall && hand == 1) {
                    1
                } else {
                    over_hand
                };
                idx = if (word == 1 && land_at == idx) {
                    if (idx <= wall) { 0 } else { idx }
                } else {
                    land_at
                };
                wait = if (word == 1 && land_at == idx) {
                    if (idx <= wall) { 0 } else { idx - wall }
                } else {
                    if (land_at > wall) { land_at - wall } else { 0 }
                };
            }
            action WallMoved when (alive == 1 && idx > 0) {
                wait = if (idx > wall) { idx - wall } else { 0 };
            }
            action CloseAndRestore when (alive == 1 && committed == 1) {
                idx = land_at;
                wait = if (land_at > wall) { land_at - wall } else { 0 };
                wake = 0;
                bit = 0;
            }
            action HandoffStart when (alive == 1 && committed == 1) {
                committed = 0;
                leaf = 1;
                leaf_word = word;
                leaf_land = land_at;
                word = 0;
                land_at = 0;
                idx = 0;
                wait = 0;
                wake = 0;
                bit = 0;
            }
            action Graft when (alive == 1 && committed == 0 && leaf == 1 && bit == 0) {
                word = leaf_word;
                land_at = leaf_land;
                idx = leaf_land;
                wait = if (leaf_land > wall) { leaf_land - wall } else { 0 };
            }
            action HandoffCommit when (committed == 0) {
                committed = 1;
                leaf = 0;
                leaf_word = 0;
                leaf_land = 0;
                idx = if (alive == 1) { land_at } else { 0 };
                wait = if (alive == 1 && land_at > wall) { land_at - wall } else { 0 };
                wake = 0;
            }
            action CloseSession when (alive == 1) {
                alive = 0;
                word = 0;
                land_at = 0;
                bit = 0;
                idx = 0;
                wait = 0;
                wake = 0;
            }

            action TrustingFire when (Buggy == 1 && alive == 1 && committed == 1 && idx > 0 && wait == 0 && idx <= wall) {
                word = 2;
                land_at = 0;
                bit = 1;
                over_hand = if (hand == 1) { 1 } else { over_hand };
                fires = if (fires > 0) { 2 } else { 1 };
            }
            action SameWordKeepsTimer when (Buggy == 1 && alive == 1 && word == 1 && land_at > 0) {
                bit = 1;
                hand = 1;
            }
            action MonotonicFire when (Buggy == 1 && alive == 1 && committed == 1 && idx > 0 && wait == 0 && word == 1 && land_at == idx) {
                word = 2;
                land_at = 0;
                idx = 0;
                bit = 1;
                early = if (idx > wall) { 1 } else { early };
                fires = if (fires > 0) { 2 } else { 1 };
            }
            action WireLandKeepsTimer when (Buggy == 1 && alive == 1 && land_at > 0) {
                word = 2;
                wake = 1;
                bit = 1;
                hand = 1;
            }
            action CommitWithoutRebuild when (Buggy == 1 && committed == 0) {
                committed = 1;
                leaf = 0;
                leaf_word = 0;
                leaf_land = 0;
                wake = 0;
            }
            action CloseKeepsEntry when (Buggy == 1 && alive == 1 && idx > 0) {
                alive = 0;
                word = 0;
                land_at = 0;
                bit = 0;
                wake = 0;
            }

            invariant TheTimerGoesWithCruise: land_at == 0 || word == 1;
            invariant LandOncePerArm: fires <= 1;
            invariant TheHandWins: over_hand == 0;
            invariant NeverEarly: early == 0;
            invariant TheIndexTrailsTheStore: alive == 0 || committed == 0 || wake == 1 || idx == land_at;
            invariant TheEntryGoesWithItsSession: alive == 1 || idx == 0;
        }
    }
}

/// AFTER THE INSTANT, LAND IS WRITTEN (`[]<>`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(autopilot_timer_model)]
pub fn autopilot_timer_liveness() -> Liveness {
    Liveness {
        name: "TheTimeLands",
        goal: or_(
            or_(gt(int(1), var("alive")), gt(int(1), var("land_at"))),
            gt(var("land_at"), var("wall")),
        ),
        weak: vec!["Fire", "WallMoved", "ApplyWake", "HandoffCommit"],
        strong: vec![],
        mutants: vec!["CommitWithoutRebuild"],
    }
}
