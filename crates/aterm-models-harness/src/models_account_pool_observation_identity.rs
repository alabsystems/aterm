// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Pool observations use the process's retained or current login and current identity policy.
/// Known contradictions supply no proof or telemetry; unknown identity remains compatible.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_pool_observation_identity_model() -> Model {
    aterm_spec::ty_model! {
        AccountPoolObservationIdentity {
            const Buggy = 0;
            var provider = 0; var default = 0; var expected = 0; var credential = 0;
            var binding = 0; var point = 0; var phase = 0;
            var accepted = 0; var proven = 0; var canary = 1; var undo = 1;
            var auth = 0; var session = 0; var sample = 0; var wall = 0;
            var unchanged = 1; var preserved = 1;
            action Provider when (phase == 0 && provider == 0) { provider = 1; }
            action Default when (phase == 0 && default == 0) { default = 1; }
            action Expected when (phase == 0 && expected <= 1) { expected = expected + 1; }
            action Credential when (phase == 0 && credential <= 1 + provider) { credential = credential + 1; }
            action Binding when (phase == 0 && binding <= 1) { binding = binding + 1; }
            action Point when (phase == 0 && point == 0) { point = 1; }
            action Observe when (phase == 0) {
                phase = 1;
                accepted = if Buggy == 1 && binding <= 1 {1}
                    else if Buggy == 2 && credential == 0 {0}
                    else if Buggy == 3 && expected == 0 {0}
                    else if Buggy == 4 && binding == 1 {0}
                    else if Buggy == 5 && binding == 2 {1}
                    else if binding <= 1 && credential <= 2 && (expected == 0 || credential == 0 || expected == credential) {1} else {0};
                proven = if Buggy == 6 && binding == 1 {1}
                    else if binding == 0 && point == 0 && credential <= 2 && (expected == 0 || credential == 0 || expected == credential) {1} else {0};
                canary = if Buggy == 7 {0}
                    else if binding == 0 && point == 0 && credential <= 2 && (expected == 0 || credential == 0 || expected == credential) {0} else {1};
                undo = if Buggy == 8 {0}
                    else if binding == 0 && point == 0 && credential <= 2 && (expected == 0 || credential == 0 || expected == credential) {0} else {1};
                auth = if Buggy == 13 {1}
                    else if binding == 0 && point == 0 && credential <= 2 && (expected == 0 || credential == 0 || expected == credential) {1} else {0};
                session = if Buggy == 9 {1}
                    else if binding <= 1 && point == 0 && credential <= 2 && (expected == 0 || credential == 0 || expected == credential) {1} else {0};
                sample = if Buggy == 10 {1}
                    else if binding <= 1 && credential <= 2 && (expected == 0 || credential == 0 || expected == credential) {1} else {0};
                wall = if Buggy == 11 {1}
                    else if binding <= 1 && point == 1 && credential <= 2 && (expected == 0 || credential == 0 || expected == credential) {1} else {0};
                unchanged = if binding <= 1 && credential <= 2 && (expected == 0 || credential == 0 || expected == credential) {0} else {1};
                preserved = if Buggy == 12 {0} else {1};
            }
            invariant Admission: phase == 0 || accepted == if binding <= 1 && credential <= 2 && (expected == 0 || credential == 0 || expected == credential) {1} else {0};
            invariant Proof: phase == 0 || proven == if binding == 0 && point == 0 && accepted == 1 {1} else {0};
            invariant Canary: canary == 1 - proven;
            invariant Undo: undo == 1 - proven;
            invariant Auth: auth == proven;
            invariant Session: phase == 0 || session == if accepted == 1 && point == 0 {1} else {0};
            invariant Sample: sample == accepted;
            invariant Wall: phase == 0 || wall == if accepted == 1 && point == 1 {1} else {0};
            invariant Records: unchanged == 1 - accepted;
            invariant Evidence: preserved == 1;
        }
    }
}
