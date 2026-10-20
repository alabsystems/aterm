// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the anchor emitters of `aterm-spec-macros`: the fn-entry probe
//! `#[refines]` injects keeps every awkward fn shape's behaviour and records the anchor
//! id, and every record the attributes submit is collected exactly once. The emitted
//! records reference `::aterm_spec_anchor::{registry,xref}::…`, so this test links
//! `aterm-spec-anchor` and `aterm-spec` (dev-dependencies; the cycle is dev-only and
//! Cargo-legal).

use aterm_spec_macros::{refines, spec_unmodeled};

// The suite, one file for the two implementations: aterm-spec-anchor's tests/attrs.rs
// mounts it under that crate's declarative attributes, and both pin the same digest.
#[path = "anchors/suite.rs"]
mod suite;
