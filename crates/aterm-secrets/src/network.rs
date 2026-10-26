// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local interface facts for vault retry decisions, without a network connection.

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use aterm_uds::network::RouteChanges;
pub use aterm_uds::network::{InterfaceAddress, interfaces};
