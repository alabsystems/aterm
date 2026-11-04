// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A retained process role survives unavailable evidence only in the same context.
/// Authentication and classification of the observed evidence are inputs to this model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn role_retention_model() -> Model {
    aterm_spec::ty_model! {
        VaultRoleRetention {
            const Buggy = 0;
            var warm = 0; var same = 1; var available = 1; var denied = 0;
            var phase = 0; var visible = 0;
            action Warm when (phase == 0 && warm == 0) { warm = 1; }
            action LoseCustody when (phase == 0 && available == 1) { available = 0; }
            action ChangeContext when (phase == 0 && same == 1) { same = 0; }
            action Deny when (phase == 0 && denied == 0) { denied = 1; }
            action Observe when (phase == 0) {
                phase = 1;
                visible = if Buggy == 1 && denied == 1 {1}
                    else if denied == 1 || Buggy == 4 {0}
                    else if available == 1 || Buggy == 2 {1}
                    else if Buggy == 5 {0}
                    else if warm == 1 && (same == 1 || Buggy == 3) {1}
                    else {0};
            }
            invariant DenialWins: phase == 0 || denied == 0 || visible == 0;
            invariant NoColdGrant: phase == 0 || available == 1 || warm == 1 || visible == 0;
            invariant SameContext: phase == 0 || available == 1 || same == 1 || visible == 0;
            invariant AvailableGrant: phase == 0 || available == 0 || denied == 1 || visible == 1;
            invariant WarmRetention: phase == 0 || warm == 0 || same == 0 || denied == 1
                || available == 1 || visible == 1;
            bound RoleBox: warm <= 1 && same <= 1 && available <= 1 && denied <= 1
                && phase <= 1 && visible <= 1;
        }
    }
}
