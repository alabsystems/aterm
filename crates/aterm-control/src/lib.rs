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
pub mod hangup;
pub mod host;
pub mod selection;
pub mod wire;

/// The verb-matrix suite every [`SessionHost`] must pass; this crate's tests
/// run it against a reference in-memory host. Compiled in every debug build,
/// so a downstream host's tests and its main-thread gate binary run it from
/// the ordinary dev target with no feature; a release build carries none of it.
#[cfg(any(test, debug_assertions))]
pub mod conformance;

pub use host::{ChangeWait, HostCapabilities, Selector, SessionEntry, SessionHost, SessionState};
