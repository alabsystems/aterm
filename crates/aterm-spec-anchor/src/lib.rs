// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! What an anchor expansion references, and nothing else.
//!
//! An anchored crate writes
//! `#[cfg_attr(any(test, debug_assertions), aterm_spec_anchor::refines(…))]` (or
//! `spec_unmodeled`). The attribute submits a record into the
//! link-time [`registry`] and, for `refines`, injects a [`xref::note_entered`]
//! probe at fn entry. A release build strips the attribute, so it carries neither.
//!
//! This crate is std-only, so depending on it costs an anchored library one small
//! compile and never the models: those live in `aterm-spec` (the engine) and the
//! `aterm-models-*` crates, which only tests depend on. `aterm-spec` re-exports
//! [`registry`] and [`xref`], so there is one registry per binary however a record
//! was submitted.

#[cfg(feature = "macros")]
pub use aterm_spec_macros::{refines, spec_unmodeled};

// The link-time collection: the `::aterm_spec_anchor::registry::submit!` the anchor
// attributes emit, and the `::aterm_spec::registry::submit!` (a re-export of this one)
// that `ty_model!` emits, resolve to the same lists.
pub mod registry;
pub mod xref;
