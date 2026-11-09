// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P1's spec island (`AtermMeshRevocation`), its Tier 2 and the spec islands of `aterm-mesh-core`
//! that open it but the largest, kernel-checked by every Trust build of this crate
//! (`cfg(clean_islands)`). `aterm-mesh-core-islands`' lib.rs says why they sit in a crate of their
//! own and who waits for it.
#![forbid(unsafe_code)]

#[cfg(clean_islands)]
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/temporal_island.rs"]
mod temporal;
#[cfg(clean_islands)]
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/revocation_island.rs"]
mod revocation;

#[cfg(clean_islands)]
mod islands;
