// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a Mac's own join of ALab says, and how long it asks the members by itself, on every
//! platform: the `members` module runs the join where it is built (unix), and System State's
//! page, built everywhere, words its Secrets line and its card by these.

/// Status while the default LAN admission is pending.
pub const LET_IN: &str = "waiting for automatic ALab admission; aterm keeps retrying";

/// The initial observation interval for an automatic ALab join. Passing it changes
/// diagnostic state, not the admission policy: the join continues without an Allow prompt.
pub const JOIN_QUIET: std::time::Duration = std::time::Duration::from_secs(30);

/// What a join that asked the members says while it asks by itself ([`JOIN_QUIET`]).
pub const JOINING: &str = "Joining ALab…";
