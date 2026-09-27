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
//! This crate provides the type-level machinery that lets the Rust compiler
//! refuse to let PTY-origin data reach a host-privileged sink without passing
//! through a named `authorize_*` ceremony that consumes a zero-sized capability
//! token.
//!
//! The Terminal-class RCE (#7875) was fixed with one such ceremony
//! (`ConductorActivationToken`). This framework generalizes that pattern.
//!
//! # Overview
//!
//! * [`Origin`] is a sealed trait implemented by 6 marker types forming the
//!   lattice described in `designs/2026-04-19-provenance-framework.md` §3:
//!   [`Host`], [`ConfigFile`], [`User`], [`Ai`], [`NetworkUntrusted`], [`Pty`];
//!   [`OriginTag`] is its runtime-shaped mirror (a plain `#[repr(u8)]` enum).
//! * [`Provenance<T, O>`] wraps a value with a compile-time origin marker.
//!   It is `#[repr(transparent)]`: `size_of::<Provenance<T, O>>() == size_of::<T>()`,
//!   and [`pty_wrap_ref`] views a borrowed PTY value as one without copying.
//! * [`authorize_pty_to_host`] lifts PTY-origin data to [`Host`], consuming a
//!   zero-sized [`HostAuthorizationToken`]. No production code calls it today
//!   (`docs/HARDCORE_BACKLOG.md` P4, parked 2026-09-25).
//!
//! The rest of the Phase 0 design — the runtime-tagged `DynProvenance` with its
//! synthetic `Top`, the type-level join table, the network edge and the per-
//! subsystem drop-on-Top counters — had no production caller and was deleted on
//! 2026-09-25.

mod authorize;
mod origin;
mod provenance;
mod pty_wrap;

pub use authorize::{HostAuthorizationToken, authorize_pty_to_host};
pub use origin::{Ai, ConfigFile, Host, NetworkUntrusted, Origin, OriginTag, Pty, User};
pub use provenance::Provenance;
pub use pty_wrap::pty_wrap_ref;
