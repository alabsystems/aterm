// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Registers the `trust` tool so the `#[cfg_attr(trust_verify, trust::skip)]`
// on the publisher's one FFI call resolves. A stable build ignores this.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's shared accessibility model and native publishers.
//!
//! macOS uses NSAccessibility on real content views, Windows uses UI Automation,
//! and Linux uses AT-SPI. Each consumes the same validated tree/text model and
//! sends advertised actions back to the host.
//!
//! The Linux AT-SPI publisher serves a window's accessibility tree to screen readers
//! (Orca) over AT-SPI, the accessibility bus at-spi2-core runs. It replaces
//! accesskit_unix and its zbus/async stack with a blocking client over the
//! first-party `aterm-dbus` (docs/AUDIT-third-party-2026-10-15.md, unit a11y,
//! step A11Y.4), and what the AccessKit crates under it did with this
//! crate's own code (A11Y.5):
//!
//! * `tree`: a window's tree as a publisher reads it, and what an update
//!   changed (carved from accesskit_consumer);
//! * `text`: a text document's positions, ranges and navigation (the same);
//! * `atspi::semantics`: AT-SPI's view of the tree, without a bus (carved
//!   from accesskit_atspi_common and atspi-common);
//! * [`model`]: the tree model aterm-gui builds (AccessKit's, by name; A11Y.5b).

#![deny(unsafe_code)]

#[cfg(any(target_os = "linux", all(test, unix)))]
pub mod atspi;
#[cfg(target_os = "macos")]
#[allow(unsafe_code, reason = "typed main-thread NSAccessibility boundary")]
pub mod macos;
pub mod model;
#[cfg(any(target_os = "macos", windows, test))]
mod platform;
#[cfg(any(target_os = "macos", target_os = "linux", windows, all(test, unix)))]
#[cfg_attr(
    not(any(target_os = "linux", all(test, unix))),
    allow(
        dead_code,
        reason = "shared consumer operations also serve the AT-SPI backend"
    )
)]
mod text;
#[cfg(any(target_os = "macos", target_os = "linux", windows, all(test, unix)))]
#[cfg_attr(
    not(any(target_os = "linux", all(test, unix))),
    allow(
        dead_code,
        reason = "shared consumer operations also serve the AT-SPI backend"
    )
)]
mod tree;
#[cfg(windows)]
#[allow(unsafe_code, reason = "Windows UI Automation COM boundary")]
pub mod uia;
#[cfg(any(windows, test))]
#[allow(unsafe_code, reason = "typed nullable COM output slots")]
mod uia_abi;
#[cfg(any(windows, test))]
mod uia_text;

#[cfg(any(windows, test))]
mod uia_changes;
