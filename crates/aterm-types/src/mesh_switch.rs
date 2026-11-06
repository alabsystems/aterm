// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release gate for the one-mesh transport and node identities.

/// Wave B stays dormant until the R2 adoption gate is met.
pub const MESH: bool = false;

/// The first `(major, minor)` release whose table readers open operations sealed under the fold
/// (keys derived from `[everyone]` epochs, docs/DESIGN-alab-one-mesh-2026-11-05.md §4.2 M6): R2's.
/// `None` until R2 is cut, so no seat counts as reading them and the fold's adoption gate holds
/// only by its bound. B14b sets it, with [`R2_SHIPPED_MS`] and [`MESH`], in R2's commit.
pub const FOLD_READS: Option<(u64, u64)> = None;

/// When R2 shipped (Unix ms): the fold's adoption gate holds 60 days after it whatever the seats
/// run. `None` until R2 is cut.
pub const R2_SHIPPED_MS: Option<u64> = None;
