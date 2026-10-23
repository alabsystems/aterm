// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control socket's image-path confinement.

use aterm_spec::derive::Model;

/// One requested write, decided by `control_auth::confine_image_path` and committed by the
/// retained-handle writer.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn path_confine_model() -> Model {
    aterm_spec::ty_model! {
        PathConfine {
            const LinkOutside = 0;
            const Buggy = 0;
            scenario LinkOutside = 1;
            var decided = 0;
            var committed = 0;
            var target = 0;

            action Confine when (decided == 0) {
                decided = 1;
                committed = if LinkOutside == 0 || Buggy == 1 { 1 } else { 0 };
                target = if LinkOutside == 0 { 1 } else { if Buggy == 1 { 2 } else { 0 } };
            }

            invariant WriteWithinSubdir: committed == 0 || target == 1;
            invariant EscapeRejected: LinkOutside == 0 || committed == 0;
        }
    }
}
