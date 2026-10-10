// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![deny(unsafe_op_in_unsafe_fn)]
// Trust tool-attribute plumbing (the scrollback/lz4 pattern): registers the
// `trust` tool namespace under verify builds only, so `#[trust::contract_panic]`
// declarations compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
#![deny(clippy::all)]

//! TLA+ specification coverage tracking for aterm formal verification.
//!
//! Cross-references Rust code against TLA+ specifications, tracking which TLA+
//! actions have corresponding Rust refinements.
//!
//! # Derived, drift-free TLA+ ([`derive`] + [`ty_model`])
//!
//! Prefer DERIVED models over hand-written `.tla`: write a bounded state machine
//! once as a Rust [`derive::Model`] (or via the [`ty_model`] macro) and the
//! `ty`-checkable spec AND the executable interpreter are generated from that one
//! source, so they cannot drift from each other. The model itself is authored, so
//! it can drift from the code; Tier-1 conformance is what binds the two. See the
//! [`derive`] module docs and the `AGENTS.md` at the repo root for the workflow;
//! [`xref::model_registry`] lists every model.

// The `ty_model!` proc-macro emits absolute `::aterm_spec::derive::*` paths so it
// works from any downstream crate. This self-alias lets aterm-spec ALSO invoke its
// own re-exported macro internally (e.g. `derive::read_image_seq_model`), so a
// derived model can be authored as `ty_model!` here, not just in dependent crates.
extern crate self as aterm_spec;

pub use aterm_spec_macros::{refines, spec_invariant, spec_unmodeled, ty_model};

// Re-export `inventory` so the proc-macro-generated `::aterm_spec::inventory::submit!`
// resolves in ANY downstream crate that uses `#[refines]`/`#[spec_unmodeled]`
// without that crate needing its own `inventory` dependency. The collected slices
// live in `xref` (TRUST_NATIVE_TLA §2.1).
pub use inventory;

mod compiled;
pub mod derive;
pub mod interp;
pub mod ir;
pub mod rng;
pub mod tla_check;
pub mod verify;
// `std::thread::sleep` and `Instant::now` panic on the browser target.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod wait;
pub mod xref;

/// Refinement mapping trait.
///
/// A concrete implementation type `C` that `impl Refines<A>` can project
/// its state to the abstract TLA+ model type `A`. This projection is the
/// Rust analogue of a TLA+ refinement mapping: every reachable concrete
/// state maps to a valid abstract state.
///
/// The trait is intentionally minimal — it exists to document and enforce
/// the correspondence, not to drive runtime behavior.
pub trait Refines<Abstract> {
    /// Project the concrete state to the abstract model.
    fn project(&self) -> Abstract;
}
