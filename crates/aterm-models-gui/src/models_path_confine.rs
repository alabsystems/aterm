// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control socket's image-path confinement.
//!
//! The AI's read+write channel is a security boundary: an `image`/snapshot write must land
//! inside its allowed subdirectory. The confused-deputy escape this pins: the old guard
//! checked only the requested path's parent, so a symlink planted at the final component
//! passed the prefix check and the writer followed it outside the root.

use aterm_spec::derive::Model;

/// One requested write, decided by `control_auth::confine_image_path` and committed by
/// the retained-handle writer.
///
/// `LinkOutside` is the adversary: 1 when the final component is a symlink resolving
/// outside the root (the `scenario`), 0 for an honest request (committed). State:
/// `decided`; `committed` (a write landed); `target` 0 none, 1 inside, 2 outside.
///
/// Mutant: `Buggy = 1` trusts the requested path and follows the link.
///
/// Tier-0: `aterm-models-gui/tests/derived_isolation.rs`. Tier-1:
/// `aterm-gui/src/path_confine_conformance_tests.rs` drives the real confine and writer
/// over an honest request and a planted escape link, with an outside commit as its
/// negative control.
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

            // Every committed write landed inside the allowed subdirectory.
            invariant WriteWithinSubdir: committed == 0 || target == 1;
            // A request that resolves outside the root is rejected: it commits nothing.
            invariant EscapeRejected: LinkOutside == 0 || committed == 0;
        }
    }
}
