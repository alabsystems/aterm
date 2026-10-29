// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Known token users agree before stored credentials inform identity or usage.
/// This checks local claim consistency, not JWT signature authenticity.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn codex_stored_identity_model() -> Model {
    aterm_spec::ty_model! {
        CodexStoredIdentity {
            const Buggy = 0;
            var phase = 0; var id = 0; var access = 0; var decoded = 1;
            var admitted = 0; var identity = 0; var usage = 0; var requests = 0; var prepared = 0;
            var preserved = 1; var scoped = 1; var details = 1;
            action AdvanceId when (phase == 0 && decoded == 1 && id <= 1) { id = id + 1; }
            action AdvanceAccess when (phase == 0 && decoded == 1 && access <= 1) { access = access + 1; }
            action Opaque when (phase == 0 && decoded == 1 && id == 0 && access == 0) { decoded = 0; }
            action Read when (phase == 0) {
                phase = 1;
                admitted = if Buggy == 1 {1}
                    else if Buggy == 8 && (id == 0 || access == 0) {0}
                    else if id == 0 || access == 0 || id == access {1} else {0};
                identity = if Buggy == 2 && id > 0 && access > 0 && (id > access || access > id) {1}
                    else if Buggy == 9 && decoded == 0 {1}
                    else if decoded == 1 && (id == 0 || access == 0 || id == access) {1} else {0};
                usage = if Buggy == 3 && id > 0 && access > 0 && (id > access || access > id) {1}
                    else if Buggy == 10 && decoded == 0 {0}
                    else if id == 0 || access == 0 || id == access {1} else {0};
                requests = if Buggy == 4 {1}
                    else if id == 0 || access == 0 || id == access {1} else {0};
                prepared = if Buggy == 11 {1}
                    else if id == 0 || access == 0 || id == access {1} else {0};
                preserved = if Buggy == 5 {0} else {1};
                scoped = if Buggy == 6 {0} else {1};
                details = if Buggy == 7 {0} else {1};
            }
            invariant Admission: phase == 0 || admitted == if id == 0 || access == 0 || id == access {1} else {0};
            invariant Identity: phase == 0 || identity == if decoded == 1 && (id == 0 || access == 0 || id == access) {1} else {0};
            invariant Usage: phase == 0 || usage == if id == 0 || access == 0 || id == access {1} else {0};
            invariant Requests: phase == 0 || requests == if id == 0 || access == 0 || id == access {1} else {0};
            invariant Preparation: phase == 0 || prepared == if id == 0 || access == 0 || id == access {1} else {0};
            invariant Evidence: preserved == 1;
            invariant Scope: scoped == 1;
            invariant Details: details == 1;
        }
    }
}
