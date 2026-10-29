// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! The model engine: [`ty_model`], the [`mod@derive`] types, the interpreter, [`verify`], and
//! [`xref`].

#![allow(
    clippy::disallowed_types,
    reason = "the specification checker's runs of external model checkers: no shipped path calls them"
)]
extern crate self as aterm_spec;

#[cfg(feature = "anchors")]
pub use aterm_spec_macros::{liveness, model, ty_model};

pub mod check;
mod compiled;
pub mod derive;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod fs_lane;
pub mod host;
pub mod interp;
pub use aterm_spec_anchor::registry;
pub mod rng;
#[cfg(test)]
mod test_models;
pub mod verify;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod wait;
pub mod walk;
pub mod xref;

/// Refinement mapping from a concrete state to its abstract TLA+ model state.
pub trait Refines<Abstract> {
    /// Project the concrete state to the abstract model.
    fn project(&self) -> Abstract;
}
