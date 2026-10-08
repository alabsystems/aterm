// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The headless half of aterm's control protocol: verb bodies that read and
//! drive a session through the [`SessionHost`] trait instead of through
//! `aterm-gui`'s winit event loop, `Store` and subscriber registry.
//!
//! The dispatcher lives in the GUI crate that owns the window, so the verb
//! bodies are parameterized over a host: a second host (a daemon with panes
//! but no frame source) answers the same wire, and [`HostCapabilities`] turns
//! a missing facility into `ERR unsupported`. This crate has no winit
//! dependency.

pub mod cast_drift;
pub mod host;
pub mod selection;
pub mod wire;

/// The verb-matrix suite every [`SessionHost`] must pass, plus the reference
/// in-memory host it is proven non-vacuous against. Always compiled under
/// `cfg(test)`; the `conformance` feature exposes it to downstream hosts.
#[cfg(any(test, feature = "conformance"))]
pub mod conformance;

pub use host::{ChangeWait, HostCapabilities, Selector, SessionEntry, SessionHost, SessionState};
