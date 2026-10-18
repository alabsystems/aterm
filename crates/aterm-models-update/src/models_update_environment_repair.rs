// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Releasing an environmental update block requires a later verified repair.

use aterm_spec::derive::Model;

#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_environment_repair_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateEnvironmentRepair {
            const Buggy = 0;
            var owned = 0;
            var fresh = 0;
            var verified = 0;
            var decided = 0;
            var latched = 1;

            action Own when (decided == 0 && owned == 0) { owned = 1; }
            action Fresh when (decided == 0 && fresh == 0) { fresh = 1; }
            action Verify when (decided == 0 && verified == 0) { verified = 1; }
            action Reduce when (decided == 0) {
                latched = if Buggy == 1 {
                    1
                } else {
                    if owned == 1 && fresh == 1 && (verified == 1 || Buggy == 2) { 0 } else { 1 }
                };
                decided = 1;
            }

            // Dial 2 releases without the verification.
            invariant ReleaseRequiresProof:
                latched == 1 || (owned == 1 && fresh == 1 && verified == 1);
            invariant RepairedSourceRecovers:
                decided == 0 || owned == 0 || fresh == 0 || verified == 0 || latched == 0;
        }
    }
}
