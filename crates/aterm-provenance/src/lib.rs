// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// `unsafe_code` is forbidden across the crate EXCEPT inside `pty_wrap` (Phase 1,
// #8005). The allowlist is narrowly scoped: `pty_wrap.rs` uses
// `#![allow(unsafe_code)]` locally for a single `#[repr(transparent)]`
// pointer reinterpret with a Kani layout-equivalence harness. Every other
// module keeps `#![deny(unsafe_code)]`.
#![deny(unsafe_code)]
#![deny(missing_docs)]
// Production unwrap() is forbidden; tests opt out uniformly at the crate root.
#![deny(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Trusted data-flow provenance framework (Phase 0; issue #8000, parent #7877).
//!
//! This crate provides the type-level taint the parser carries: every byte the
//! PTY hands the `ActionSink` arrives as `Provenance<_, Pty>`, and no conversion
//! relabels it (the `compile_fail` doctests on [`Provenance`] pin that).
//! Privileged sinks are gated by capability ZSTs minted inside dispatch
//! (`aterm-core/tests/capability_ceremony.rs`), not by a lift out of `Pty`.
//!
//! # Overview
//!
//! * [`Origin`] is a sealed trait implemented by 6 marker types:
//!   [`Host`], [`ConfigFile`], [`User`], [`Ai`], [`NetworkUntrusted`], [`Pty`];
//!   [`OriginTag`] is its runtime-shaped mirror (a plain `#[repr(u8)]` enum).
//! * [`Provenance<T, O>`] wraps a value with a compile-time origin marker.
//!   It is `#[repr(transparent)]`: `size_of::<Provenance<T, O>>() == size_of::<T>()`,
//!   and [`pty_wrap_ref`] views a borrowed PTY value as one without copying.
//!
//! The rest of the Phase 0 design had no production caller and was deleted: the
//! runtime-tagged `DynProvenance` with its synthetic `Top`, the type-level join
//! table, the network edge and the per-subsystem drop-on-Top counters on
//! 2026-09-25, and the `authorize_pty_to_host` lift with its `internal-mint`-sealed
//! token on 2026-09-27 (`docs/HARDCORE_BACKLOG.md` P4, re-scoped to the capability
//! sink tokens).

mod origin;
mod provenance;
mod pty_wrap;

pub use origin::{Ai, ConfigFile, Host, NetworkUntrusted, Origin, OriginTag, Pty, User};
pub use provenance::Provenance;
pub use pty_wrap::pty_wrap_ref;
