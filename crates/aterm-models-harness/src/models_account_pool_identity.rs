// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Known identity contradictions refuse preparation and final admission. An
/// unknown identity is compatible; each observation is not a filesystem lease.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_pool_identity_model() -> Model {
    aterm_spec::ty_model! {
        AccountPoolIdentity {
            const Buggy = 0;
            var provider = 0; var route = 0; var default = 0; var expected = 0;
            var credential = 0; var seen = 0; var phase = 0; var prepared = 0;
            var returned = 0; var rotated = 0; var unchanged = 1; var preserved = 1;
            action Provider when (phase == 0 && provider == 0) { provider = 1; }
            action Route when (phase == 0 && route <= 1) { route = route + 1; }
            action Default when (phase == 0 && default == 0) { default = 1; }
            action Expected when (phase == 0 && expected <= 1) { expected = expected + 1; }
            action Credential when (phase == 0 && credential <= 1) { credential = credential + 1; }
            action Prepare when (phase == 0) {
                seen = credential;
                prepared = if Buggy == 1 {1}
                    else if Buggy == 2 && credential == 0 {0}
                    else if expected == 0 || credential == 0 || expected == credential {1} else {0};
                phase = if expected == 0 || credential == 0 || expected == credential {1} else {2};
            }
            action Change when (phase == 1 && credential <= 1) { credential = credential + 1; }
            action Unknown when (phase == 1 && credential > 0) { credential = 0; }
            action Finish when (phase == 1) {
                phase = 2;
                returned = if Buggy == 3 {1}
                    else if Buggy == 4 {prepared}
                    else if Buggy == 5 && credential == 0 {0}
                    else if expected == 0 || credential == 0 || expected == credential {1} else {0};
                rotated = if Buggy == 6 {1} else if Buggy == 7 {0}
                    else if route == 2 && (expected == 0 || credential == 0 || expected == credential) {1} else {0};
                unchanged = if Buggy == 8 {0}
                    else if route == 2 && (expected == 0 || credential == 0 || expected == credential) {0} else {1};
                preserved = if Buggy == 9 {0} else {1};
            }
            invariant Preparation: phase == 0 || prepared == if expected == 0 || seen == 0 || expected == seen {1} else {0};
            invariant Admission: phase <= 1 || returned == if prepared == 1 && (expected == 0 || credential == 0 || expected == credential) {1} else {0};
            invariant Rotation: phase <= 1 || rotated == if route == 2 && returned == 1 {1} else {0};
            invariant Records: phase <= 1 || unchanged == 1 - rotated;
            invariant Evidence: preserved == 1;
        }
    }
}
