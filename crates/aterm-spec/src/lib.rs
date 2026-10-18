// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! The model engine: [`ty_model`], the [`derive`] types, the interpreter, [`verify`], [`xref`].
//!
//! # Derived models ([`derive`] + [`ty_model`])
//!
//! A bounded state machine is written once as a [`derive::Model`] (or with [`ty_model`]); the
//! `ty`-checkable spec and the executable interpreter are both generated from it, so they cannot
//! drift apart. The model can drift from the code: a Tier-1 conformance test binds the two (see
//! [`derive`]). [`xref::model_registry`] lists every model linked into the binary. The models
//! themselves live in the `aterm-models-<domain>` crates; `aterm-models` links them all.

#![allow(
    clippy::disallowed_types,
    reason = "the specification checker's runs of external model checkers: no shipped path calls them"
)]
// `ty_model!` emits absolute `::aterm_spec::derive::*` paths; this alias lets aterm-spec use it on
// itself too.
extern crate self as aterm_spec;

#[cfg(feature = "anchors")]
pub use aterm_spec_anchor::{refines, spec_unmodeled};
#[cfg(feature = "anchors")]
pub use aterm_spec_macros::{liveness, model, ty_model};

pub mod check;
mod compiled;
pub mod derive;
pub mod host;
pub mod interp;
// The link-time collection behind `xref` (`aterm-spec-anchor`'s, so a model `ty_model!`
// submits and an anchor `#[refines]` submits land in the same lists): the
// `::aterm_spec::registry::submit!` that `ty_model!` emits resolves in any crate that depends on
// this one.
pub use aterm_spec_anchor::registry;
pub mod rng;
// The committed models the engine's own unit tests run on, mounted from their domain crates.
#[cfg(test)]
mod test_models;
pub mod verify;
// `std::thread::sleep` and `Instant::now` panic on the browser target.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod wait;
pub mod walk;
pub mod xref;

/// Refinement mapping: an implementation projects its state to the abstract TLA+ model type, and
/// every reachable concrete state maps to a valid abstract state. It documents the correspondence;
/// it drives no runtime behaviour.
pub trait Refines<Abstract> {
    /// Project the concrete state to the abstract model.
    fn project(&self) -> Abstract;
}
