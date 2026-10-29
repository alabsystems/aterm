// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A row's explicit identity constrains each credential snapshot used for launch or usage.
/// Replacement after preparation is allowed: preparation is not a filesystem lease.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn codex_row_identity_model() -> Model {
    aterm_spec::ty_model! {
        CodexRowIdentity {
            const Buggy = 0;
            var phase = 0; var expected = 0; var email = 0; var access_email = 0;
            var prepared_email = 0; var prepared_access = 0;
            var changed = 0; var prepared = 0; var usage = 0; var requests = 0;
            var preserved = 1; var scoped = 1;
            action Expected when (phase == 0 && expected <= 1) { expected = expected + 1; }
            action Email when (phase == 0 && email <= 1) { email = email + 1; }
            action AccessEmail when (phase == 0 && access_email <= 1) { access_email = access_email + 1; }
            action Prepare when (phase == 0) {
                phase = 1; prepared_email = email; prepared_access = access_email;
                prepared = if Buggy == 1 {1}
                    else if Buggy == 2 && email == 0 {0}
                    else if expected == 0 || ((email == 0 || expected == email) && (access_email == 0 || expected == access_email)) {1} else {0};
            }
            action Replace when (phase == 1 && changed == 0) {
                changed = 1;
                access_email = if access_email <= 1 {access_email + 1} else {0};
            }
            action Poll when (phase == 1) {
                phase = 2;
                usage = if Buggy == 3 {1}
                    else if Buggy == 4 {prepared}
                    else if Buggy == 5 && email == 0 {0}
                    else if expected == 0 || ((email == 0 || expected == email) && (access_email == 0 || expected == access_email)) {1} else {0};
                requests = if Buggy == 6 {1}
                    else if expected == 0 || ((email == 0 || expected == email) && (access_email == 0 || expected == access_email)) {1} else {0};
                preserved = if Buggy == 7 {0} else {1};
                scoped = if Buggy == 8 {0} else {1};
            }
            invariant Preparation: phase == 0 || prepared == if expected == 0 || ((prepared_email == 0 || expected == prepared_email) && (prepared_access == 0 || expected == prepared_access)) {1} else {0};
            invariant Usage: phase <= 1 || usage == if expected == 0 || ((email == 0 || expected == email) && (access_email == 0 || expected == access_email)) {1} else {0};
            invariant Requests: phase <= 1 || requests == if expected == 0 || ((email == 0 || expected == email) && (access_email == 0 || expected == access_email)) {1} else {0};
            invariant Preservation: preserved == 1;
            invariant Scope: scoped == 1;
        }
    }
}
