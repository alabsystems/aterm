// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(anchor_rules, feature(macro_attr))]

//! Tests exercising `aterm-spec-anchor` attributes against the shared anchor test suite.

use aterm_spec_anchor::{refines, spec_unmodeled};

#[path = "../../aterm-spec-macros/tests/anchors/suite.rs"]
mod suite;
