// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Registers the `trust` tool so the `#[cfg_attr(trust_verify, trust::skip)]`
// on the publisher's one FFI call resolves. A stable build ignores this.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's accessibility publishing.
//!
//! [`atspi`] serves a window's accessibility tree to Linux screen readers
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

#[cfg(unix)]
pub mod atspi;
pub mod model;
#[cfg(unix)]
mod text;
#[cfg(unix)]
mod tree;
