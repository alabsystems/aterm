// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `document-features`: aterm's first-party replacement for the upstream proc
//! macro, patched in (`[patch.crates-io]`) under wgpu and wgpu-core.
//! [`document_features!`] expands to an empty string literal.
//!
//! # What the consumers do with it
//!
//! Each has exactly one call, in crate documentation:
//!
//! ```text
//! wgpu-29.0.3/src/lib.rs:64        #![doc = document_features::document_features!()]
//! wgpu-core-29.0.3/src/lib.rs:6    #![doc = document_features::document_features!()]
//! ```
//!
//! Upstream reads the consumer's `Cargo.toml` at compile time and renders its
//! `## ` feature comments as Markdown. That text reaches only rustdoc output
//! for wgpu and wgpu-core, which aterm neither builds nor ships, so the empty
//! string changes no compiled code. What leaves the graph is the proc macro and
//! its parser, litrs: 2 packages, 6,287 lines, one proc macro per wgpu cell.
//!
//! # Not equivalent
//!
//! * The feature list is missing from a local `cargo doc` of wgpu. Read it in
//!   wgpu's own Cargo.toml.
//! * Upstream rejects malformed arguments with a `compile_error!`; this
//!   accepts any input, including upstream's `feature_label = "..."`.
//! * No `self_test_helper!`: the `self-test` feature is declared and inert.
//!
//! Retires with `wgpu`.

use proc_macro::{Literal, TokenStream, TokenTree};

/// Expands to `""`, whatever the arguments. See the crate documentation.
#[proc_macro]
pub fn document_features(_args: TokenStream) -> TokenStream {
    TokenTree::Literal(Literal::string("")).into()
}
