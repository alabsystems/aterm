// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Test modules for aterm-core: property-based tests (proptest) over its
//! crate-private seams.
//!
//! Visual regression, platform glyph, and SSH conductor tests migrated to
//! `aterm-integration-tests` as part of Gate 3 (#6803).
//! Cross-crate behavioral suites live in `aterm-integration-tests`.
//! Remaining `src/tests/` coverage is white-box or aterm-core-local.

mod proptest;
