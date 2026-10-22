// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! agh's models (docs/DESIGN-agh-2026-10-19.md §17): the route wall in M1. Writing a model
//! registers it; `aterm-models` links this crate for spec-check and the registry sweeps, and
//! agh's in-crate `src/policy_conformance_tests.rs` binds it to the shipping router and hop.

mod models_route_wall;

pub use models_route_wall::*;
