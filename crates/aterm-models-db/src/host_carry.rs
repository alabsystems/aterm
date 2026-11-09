// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Re-encrypting one host register must preserve the selected semantic value,
/// semantic operation and unresolved conflicts across overlapping audiences.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn host_carry_model() -> Model {
    aterm_spec::ty_model! {
        DbHostCarry {
            const Buggy = 0;
            var ordered = 0; var done = 0;
            var value = 1; var operation = 1; var conflicts = 1;
            action Ordered when (done == 0 && ordered == 0) { ordered = 1; }
            action Read when (done == 0) {
                done = 1;
                value = if Buggy == 1 {0} else {1};
                operation = if Buggy == 2 {0} else {1};
                conflicts = if Buggy == 3 {0} else {1};
            }
            invariant ValuePreserved: value == 1;
            invariant SemanticOperationPreserved: operation == 1;
            invariant ConflictPreserved: conflicts == 1;
        }
    }
}
