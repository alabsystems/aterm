// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
// The anchor attributes are `macro_rules!` attributes where Cargo resolved no proc macro
// under them (anchor_rules.rs).
#![cfg_attr(anchor_rules, feature(macro_attr))]

//! What an anchor expansion references, and the anchor attributes themselves.
//!
//! An anchored crate writes
//! `#[cfg_attr(any(test, debug_assertions), aterm_spec_anchor::refines(…))]` (or
//! `spec_unmodeled`). The attribute submits a record into the
//! link-time [`registry`] and, for `refines`, puts a [`xref::note_entered`] probe at
//! fn entry. A release build strips the attribute, so it carries neither.
//!
//! The attributes come in two implementations with one expansion, chosen by
//! `cfg(anchor_rules)` (anchor_rules.rs), the fact Cargo resolves this crate's
//! dependencies by:
//!
//! * where a Trust compiler's cfg query answers `trust_verify` (its build, test, check,
//!   doc and verified lanes): `macro_rules!` attributes defined here, so no proc macro
//!   is in any anchored library's graph and Trust's PM-1 boundary verifies it. Every
//!   anchored crate turns on `#![cfg_attr(anchor_rules, feature(macro_attr))]`;
//! * on a stock compiler (the cross cells, the public snapshot) and under Trust's lint
//!   driver: `aterm-spec-macros`' proc macros, re-exported. `macro_attr` is not stable,
//!   and the lint gate reads the expansion it always read.
//!
//! Both emit the same records: tests/attrs.rs runs the proc macros' own suite against
//! the attributes exported here and pins the same digest. Two differences remain, both
//! in the declarative attributes. A record's `location` in a stack of anchors on one fn
//! is the line of the stack's first attribute (the one-step expansion re-emits the rest).
//! And the rebuilt fn is spanned by the expansion, so rustc's item-level lints
//! (`dead_code`, `missing_docs`) do not fire on it in those lanes; the lint gate, on the
//! proc macros, still sees them.
//!
//! This crate is std-only, so depending on it costs an anchored library one small
//! compile and never the models: those live in `aterm-spec` (the engine) and the
//! `aterm-models-*` crates, which only tests depend on. `aterm-spec` re-exports
//! [`registry`] and [`xref`], so there is one registry per binary however a record
//! was submitted.

// Stock compilers and the lint driver: the proc macros.
#[cfg(not(anchor_rules))]
pub use aterm_spec_macros::{refines, spec_unmodeled};

// Everywhere else on Trust: the declarative attributes, exported at the crate root.
#[cfg(anchor_rules)]
mod attrs;

// The link-time collection: the `::aterm_spec_anchor::registry::submit!` the anchor
// attributes emit, and the `::aterm_spec::registry::submit!` (a re-export of this one)
// that `ty_model!` emits, resolve to the same lists.
pub mod registry;
pub mod xref;
