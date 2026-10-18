// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! TOOLCHAIN LEASES: a run holds the store build it resolved for exactly as long as it
//! runs. The protocol is `aterm_toolchain::lease` (std only, so `aterm-verify` takes the
//! same lease without atpkg), re-exported here under the path atpkg's gc, seam, quiet
//! flip, doctor and holder verb, and the release cutter, always named; [`Subject::path`]
//! takes a [`Layout`], which is its prefix.

use std::path::Path;

pub use aterm_toolchain::lease::*;

use crate::store::Layout;

/// A layout is its prefix, so `subject.path(layout)` names where the subject lives.
impl AsRef<Path> for Layout {
    fn as_ref(&self) -> &Path {
        &self.prefix
    }
}
