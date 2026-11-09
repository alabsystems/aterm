// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Empty-pool and ordinary roster sync share a fleet receipt through seven durable cuts.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_fleet_commit_model() -> Model {
    aterm_spec::ty_model! {
        AccountFleetCommit {
            const Buggy = 0;
            var cut = 0; var fault = 0; var phase = 0; var ordinary = 0;
            var completed = 0; var fenced = 1; var preserved = 1;
            var exact = 1; var once = 1; var target_bound = 1;
            action Ordinary when (phase == 0 && ordinary == 0) { ordinary = 1; }
            action Cut when (phase == 0 && 6 > cut) { cut = cut + 1; }
            action Fault when (phase == 0 && 4 > fault) { fault = fault + 1; }
            action Recover when (phase == 0) {
                phase = 1;
                completed = if Buggy == 1 {1} else {if Buggy == 2 {0} else {if fault == 0 {1} else {0}}};
                fenced = if Buggy == 3 {0} else {1};
                preserved = if Buggy == 4 {0} else {1};
                exact = if Buggy == 5 {0} else {1};
                once = if Buggy == 6 {0} else {1};
                target_bound = if Buggy == 7 {0} else {1};
            }
            invariant ConflictRefused: completed == 0 || fault == 0;
            invariant ValidCutCompletes: phase == 0 || fault > 0 || completed == 1;
            invariant PendingFencesReaders: fenced == 1;
            invariant OutsideEditsPreserved: preserved == 1;
            invariant ExactImages: exact == 1;
            invariant ReplayIdempotent: once == 1;
            invariant CallerBindsTarget: target_bound == 1;
            bound CutDomain: 0 <= cut && cut <= 6;
            bound FaultDomain: 0 <= fault && fault <= 4;
            bound OrdinaryDomain: 0 <= ordinary && ordinary <= 1;
        }
    }
}
