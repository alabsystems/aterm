// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The no-lost-edit spec island of `aterm-mesh-core` (`AtermMeshNoLostEdit`), its largest, with
//! the temporal prelude and P1's island it opens, kernel-checked by every Trust build of this
//! crate (`cfg(clean_islands)`). `aterm-mesh-core-islands`' lib.rs says why it sits in a crate of
//! its own and who waits for it.
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
