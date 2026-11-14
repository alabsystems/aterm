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
pub use aterm_spec_macros::{liveness, model};

/// Author a model whose declarations share the emitted TLA+ namespace.
/// Generated names (`vars`, `Init`, `Next`, `Spec`) cannot be redeclared.
///
/// ```compile_fail
/// let _ = aterm_spec::ty_model! { Collision {
///     var count = 0;
///     action Next { count = count; }
///     invariant Bounded: count == 0;
/// } };
/// ```
///
/// Names cannot be reused across declaration roles either.
///
/// ```compile_fail
/// let _ = aterm_spec::ty_model! { Collision {
///     const Bound = 0;
///     var count = 0;
///     action Advance { count = count; }
///     invariant Bound: count == 0;
/// } };
/// ```
#[cfg(feature = "anchors")]
pub use aterm_spec_macros::ty_model;

mod bound_projection;
pub mod check;
mod compiled;
pub mod derive;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod fs_lane;
pub mod host;
pub mod interp;
pub use aterm_spec_anchor::registry;
pub mod rng;
#[cfg(unix)]
pub mod start;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod subprocess_lane;
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
