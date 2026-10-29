// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Both identity fields describe the captured bytes, despite a later file change.
/// This does not model an in-place writer modifying bytes during the read itself.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_identity_snapshot_model() -> Model {
    aterm_spec::ty_model! {
        AccountIdentitySnapshot {
            const Buggy = 0;
            var kind = 0; var change = 0; var legacy = 0; var phase = 0;
            var email = 0; var principal = 0; var intact = 1;
            action Kind when (phase == 0 && kind <= 3) { kind = kind + 1; }
            action Change when (phase == 0 && change <= 2) { change = change + 1; }
            action Legacy when (phase == 0 && legacy == 0) { legacy = 1; }
            action Observe when (phase == 0) {
                phase = 1;
                email = if Buggy == 2 && change > 0 {
                    if change == 3 {0} else {2}
                } else if Buggy == 3 && (kind == 2 || kind == 4) {1}
                else if kind == 0 || kind == 3 {1} else if kind == 1 {2} else {0};
                principal = if Buggy == 1 && change > 0 {
                    if change == 3 {0} else {2}
                } else if Buggy == 4 && kind == 3 {1}
                else if kind == 0 || kind == 2 {1} else if kind == 1 {2} else {0};
                intact = if Buggy == 5 {0} else {1};
            }
            invariant Email: phase == 0 || email == if kind == 0 || kind == 3 {1} else if kind == 1 {2} else {0};
            invariant Principal: phase == 0 || principal == if kind == 0 || kind == 2 {1} else if kind == 1 {2} else {0};
            invariant ReadOnly: intact == 1;
        }
    }
}
