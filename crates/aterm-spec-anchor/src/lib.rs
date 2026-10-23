// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
#![cfg_attr(anchor_rules, feature(macro_attr))]

//! Specification refinement anchor attributes and link-time registry.

#[cfg(not(anchor_rules))]
pub use aterm_spec_macros::{refines, spec_unmodeled};

#[cfg(anchor_rules)]
mod attrs;

pub mod registry;
pub mod xref;
