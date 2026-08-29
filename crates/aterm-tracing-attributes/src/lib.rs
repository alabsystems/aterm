// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `#[instrument]` for aterm's first-party `tracing` shim — a pass-through.
//!
//! This crate exists for one structural reason: a crate cannot be both a
//! `proc-macro` crate and a normal library, so the shim in `crates/aterm-tracing`
//! (package `tracing`) cannot define an attribute macro itself. Upstream
//! `tracing` has exactly the same problem and solves it exactly this way — it
//! depends on `tracing-attributes` and re-exports `instrument` from its crate
//! root behind the default `attributes` feature. We mirror that shape so
//! `use tracing::instrument;` (zbus, two modules) and `#[tracing::instrument]`
//! both resolve to the same path they resolve to upstream.
//!
//! # What the macro does, and why doing nothing is right
//!
//! Upstream's `#[instrument]` rewrites the annotated function so its body runs
//! inside a span. aterm installs no `tracing` subscriber anywhere — see the
//! module docs of the `tracing` shim for the verification command and the full
//! argument — so upstream's generated span is entered against `NoSubscriber`,
//! records nothing, and emits nothing. The observable difference between "wrap
//! the body in a span that goes nowhere" and "leave the body alone" is zero, so
//! this macro returns the item's `TokenStream` byte-for-byte unchanged and
//! throws the attribute arguments away.
//!
//! Throwing the arguments away is not laziness, it is the contract: the shim
//! must never evaluate what a disabled callsite would not evaluate. There is
//! nothing in `skip(self)`, `skip_all`, `level = "trace"` or
//! `name = "socket reader"` that a disabled callsite acts on.
//!
//! # Consequences worth knowing before you rely on this
//!
//! * `#[instrument]` here does **not** validate its arguments. Upstream rejects
//!   `skip(nonexistent_field)` at compile time; we accept any balanced token
//!   tree. A typo in an attribute argument is silently fine — which is the
//!   price of not depending on `syn`.
//! * Because the item comes back untouched, `#[instrument]` composes with
//!   anything (async fns, inherent methods, generics, `#[allow]` above or
//!   below it) without the ordering hazards a rewriting macro has.
//! * If a subscriber is ever installed, this attribute produces no spans. See
//!   the shim's "retiring the shim" note.

use proc_macro::TokenStream;

/// Pass-through `#[instrument]`: returns the annotated item unchanged.
///
/// The attribute arguments are accepted and discarded. Every argument shape
/// aterm's dependency graph actually uses is covered by "any balanced token
/// tree": `#[instrument]`, `#[instrument(skip(self))]`,
/// `#[instrument(skip_all, level = "trace")]`,
/// `#[instrument(name = "socket reader", skip(self), level = "trace")]`.
///
/// ```ignore
/// use tracing::instrument;
///
/// #[instrument(skip_all, level = "trace")]
/// async fn read_frame(&mut self) -> Result<Frame> { /* body unchanged */ }
/// ```
#[proc_macro_attribute]
pub fn instrument(_args: TokenStream, item: TokenStream) -> TokenStream {
    // `item` is handed straight back. Not `item.to_string().parse()`, not a
    // re-emitted `quote!` — the same TokenStream, so spans, hygiene and
    // diagnostics in the annotated function point exactly where they did
    // before the attribute was applied.
    item
}
