// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The headless half of aterm's control protocol: verb bodies that read and
//! drive a session through the [`SessionHost`] trait instead of through
//! `aterm-gui`'s winit event loop, `Store` and subscriber registry.
//!
//! WHY A TRAIT AND NOT A MOVE: the protocol is already right, but the SERVER is
//! not extractable — the dispatcher lives in a GUI crate that owns the window.
//! Parameterizing the verb bodies over a host lets a second host (a daemon with
//! panes but no frame source) answer the SAME wire without a dialect, and lets
//! [`HostCapabilities`] turn "this host cannot do that" into an honest
//! `ERR unsupported` rather than a plausible-looking empty answer.
//!
//! This crate carries NO winit dependency by construction; that is the property
//! the extraction exists to create.

pub mod host;
pub mod selection;
pub mod wire;

/// The verb-matrix suite every [`SessionHost`] must pass, plus the reference
/// in-memory host it is proven non-vacuous against. Always compiled under
/// `cfg(test)`; the `conformance` feature exposes it to downstream hosts.
#[cfg(any(test, feature = "conformance"))]
pub mod conformance;

pub use host::{ChangeWait, HostCapabilities, Selector, SessionEntry, SessionHost, SessionState};
