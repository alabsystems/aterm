// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// `unsafe_code` is denied across the crate EXCEPT inside `pty_wrap` (Phase 1,
// #8005). The allowlist is narrowly scoped: `pty_wrap.rs` uses
// `#![allow(unsafe_code)]` locally for a single `#[repr(transparent)]`
// pointer reinterpret, pinned by its layout-equivalence tests. Every other
// module keeps `#![deny(unsafe_code)]`.
#![deny(unsafe_code)]
#![warn(missing_docs)]
// Production unwrap() is forbidden; tests opt out uniformly at the crate root.
#![warn(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Trusted data-flow provenance framework (Phase 0; issue #8000, parent #7877).
//!
//! This crate provides the type-level taint the parser carries: every byte the
//! PTY hands the `ActionSink` arrives as `Provenance<_, Pty>`, and no conversion
//! relabels it.
//! Privileged sinks are gated by capability ZSTs minted inside dispatch
//! (`aterm-core/src/terminal/*_auth.rs`, each sealed by a private field,
//! `aterm-core/src/terminal/mod.rs`'s `Seal`), not by a lift out of `Pty`.
//!
//! # Overview
//!
//! * [`Origin`] is a sealed trait implemented by the [`Pty`] marker.
//! * [`Provenance<T, O>`] wraps a value with a compile-time origin marker.
//!   It is `#[repr(transparent)]`: `size_of::<Provenance<T, O>>() == size_of::<T>()`,
//!   and [`pty_wrap_ref`] views a borrowed PTY value as one without copying.

mod origin;
mod provenance;
mod pty_wrap;

pub use origin::{Origin, Pty};
pub use provenance::Provenance;
pub use pty_wrap::pty_wrap_ref;
