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
//! Provides tools to cross-reference Rust Kani proofs against TLA+ specifications,
//! tracking which TLA+ actions have corresponding Rust refinements.
//!
//! # Derived, drift-free TLA+ ([`derive`] + [`ty_model`])
//!
//! Prefer DERIVED models over hand-written `.tla`: write a bounded state machine
//! once as a Rust [`derive::Model`] (or via the [`ty_model`] macro) and the
//! `ty`-checkable spec AND the executable interpreter are generated from that one
//! source, so they cannot drift from each other or from the code. See
//! `docs/RFC-ty-embed-derived-tla.md` and the `AGENTS.md` at the repo root for the
//! workflow (Tier-0 `ty check` of the derived spec; Tier-1 conformance binding the
//! model to the real code). Existing models: [`derive::ring_model`],
//! [`derive::kernel_model`], [`derive::subscribe_model`], [`derive::snapshot_model`],
//! [`derive::transact_model`], [`derive::cursor_model`],
//! [`derive::read_image_seq_model`] (the A-3 read_image snapshot-seq protocol),
//! and the native tab-app contracts [`derive::native_control_routing_model`],
//! [`derive::native_tab_identity_model`],
//! [`derive::native_settings_singleton_model`],
//! [`derive::native_settings_draft_close_model`],
//! [`derive::manual_config_handoff_model`],
//! [`derive::native_packages_worker_model`],
//! [`derive::native_config_transaction_model`],
//! [`derive::native_config_observation_handoff_model`],
//! [`derive::native_document_publication_model`],
//! [`derive::native_close_plan_model`], [`derive::native_async_delivery_model`],
//! and [`derive::native_updater_model`].

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

pub mod coverage;
pub mod derive;
pub mod interp;
pub mod ir;
pub mod tla_check;
pub mod verify;
pub mod xref;

/// Bind a `#[kani::proof]` harness to a model `(machine, action)` — the kani half of
/// the unified verifier ledger (TRUST_NATIVE_TLA §4, Phase 4).
///
/// ```ignore
/// aterm_spec::proof_anchor!(machine = "Ring", action = "Push", proof = "ring_push_bounded");
/// ```
///
/// Expands to an `inventory::submit!` of a [`xref::ProofAnchor`] so the
/// `spec_xref_closure` gate collects it (cross-crate) into ONE per-action ledger spanning
/// `ty` (temporal) and `kani` (bounded-local). The named `(machine, action)` is held to
/// the SAME Ob.1/Ob.4 obligations as a `#[refines]` anchor — a bogus action fails the gate.
///
/// # The decoupling that makes this work (the §4 subtlety)
///
/// kani harnesses are `#[cfg(kani)]`-gated — DORMANT under stock `cargo`. A
/// `#[proof_anchors]` ATTRIBUTE on such a fn would be stripped (never registering) in
/// normal/test builds. So this is a MODULE-LEVEL declarative-macro INVOCATION, placed
/// OUTSIDE any `#[cfg(kani)]` block and decoupled from the harness (which it names by
/// string). Gate the invocation with `#[cfg(any(test, feature = "spec-anchors"))]` —
/// exactly like `#[cfg_attr(…, refines(…))]` — so it is collected in the gate's test build
/// but byte-absent from production. Do NOT place it inside the `#[cfg(kani)]` harness module.
#[macro_export]
macro_rules! proof_anchor {
    (machine = $machine:literal, action = $action:literal, proof = $proof:literal $(,)?) => {
        $crate::inventory::submit! {
            $crate::xref::ProofAnchor {
                machine: $machine,
                action: $action,
                proof_name: $proof,
                kind: $crate::xref::ProofKind::Kani,
                location: concat!(file!(), ":", line!()),
            }
        }
    };
}

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
