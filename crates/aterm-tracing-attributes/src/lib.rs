// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pass-through `#[instrument]` for aterm's `tracing` shim
//! (`crates/aterm-tracing`).
//!
//! A separate crate because a proc-macro crate cannot also be a normal library;
//! upstream splits `tracing` / `tracing-attributes` the same way, and the shim
//! re-exports `instrument` at its root so `use tracing::instrument;` and
//! `#[tracing::instrument]` resolve as upstream.
//!
//! aterm installs no subscriber (the shim's module docs carry the argument), so
//! upstream's generated span records nothing. Returning the item unchanged and
//! discarding the arguments is observably identical.
//!
//! * Arguments are not validated: `skip(nonexistent_field)` is accepted, the
//!   price of not depending on `syn`.
//! * The untouched item composes with anything (async fns, methods, generics,
//!   other attributes) without a rewriting macro's ordering hazards.
//! * If a subscriber is ever installed, this produces no spans; retire the shim.

use proc_macro::TokenStream;

/// Pass-through `#[instrument]`: returns the annotated item unchanged and
/// discards the arguments (any balanced token tree).
///
/// ```ignore
/// use tracing::instrument;
///
/// #[instrument(skip_all, level = "trace")]
/// async fn read_frame(&mut self) -> Result<Frame> { /* body unchanged */ }
/// ```
#[proc_macro_attribute]
pub fn instrument(_args: TokenStream, item: TokenStream) -> TokenStream {
    // The same TokenStream, not re-parsed, so spans and diagnostics in the
    // annotated item point where they did before.
    item
}
