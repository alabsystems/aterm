// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(anchor_rules, feature(macro_attr))]

//! The anchor attributes this crate exports, under the proc macros' own suite: the same
//! shapes (an early return, a generic, a `const fn`, an inner attribute, `self` by value,
//! four anchors on one method), the same probes, and the same digest over every record
//! the binary collects. Where Cargo resolved `cfg(anchor_rules)` these are the
//! `macro_rules!` attributes, so a drift from the proc macros' records fails here.

use aterm_spec_anchor::{refines, spec_unmodeled};

#[path = "../../aterm-spec-macros/tests/anchors/suite.rs"]
mod suite;
