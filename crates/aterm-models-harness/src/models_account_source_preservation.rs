// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Automatic imports preserve absent and existing source files across receipt cuts.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_source_preservation_model() -> Model {
    aterm_spec::ty_model! {
        AccountSourcePreservation {
            const Buggy = 0;
            var kind = 0; var cut = 0; var done = 0;
            var preserved = 1; var pending = 0; var published = 0; var recovered = 0;
            action Source when (done == 0 && 4 > kind) { kind = kind + 1; }
            action Cut when (done == 0 && 2 > cut) { cut = cut + 1; }
            action Sync when (done == 0) {
                done = 1;
                preserved = if (cut > 0 && ((Buggy == 1 && kind == 0) || (Buggy == 2 && kind == 2)))
                    || (Buggy == 3 && kind == 4 && cut == 2) {0} else {1};
                pending = if cut == 1 && (4 > kind || Buggy == 3) {1} else {0};
                published = if cut == 2 && (4 > kind || Buggy == 3) {1} else {0};
                recovered = if cut == 1 && (4 > kind || Buggy == 3) {1} else {0};
            }
            invariant SourcePreserved: preserved == 1;
            bound PreservationDomain: kind <= 4 && cut <= 2 && done <= 1
                && preserved <= 1 && pending <= 1 && published <= 1 && recovered <= 1;
        }
    }
}
