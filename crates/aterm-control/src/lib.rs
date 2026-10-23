// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless control-protocol verb bodies parameterized over [`SessionHost`].

pub mod asciicast;
pub mod cast_drift;
pub mod hangup;
pub mod host;
pub mod park;
pub mod replay;
pub mod selection;
pub mod wire;

/// The verb-matrix suite every [`SessionHost`] must pass.
#[cfg(any(test, debug_assertions))]
pub mod conformance;

pub use host::{ChangeWait, HostCapabilities, Selector, SessionEntry, SessionHost, SessionState};
