// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The spec islands of `aterm-mesh-core` (`docs/DESIGN-one-mesh-proofs-2026-11-05.md`),
//! kernel-checked by every Trust build of this crate (`cfg(clean_islands)`).
//!
//! The islands prove the laws of aterm-mesh-core's kernels and name no Rust, so nothing ties
//! their check to the kernels' compile. Trust checks a crate's islands before it writes the
//! crate's metadata, which every dependent waits for: in aterm-mesh-core they held aterm-link,
//! aterm-mesh, aterm-agent, aterm-gui and the `aterm` link behind three minutes of kernel work
//! (`docs/measured/mesh-core-islands-off-the-chain-2026-11-08.md`). Here only what needs their
//! verdict waits:
//!
//! - the `aterm` package, so every build of the product checks them, a release's slices included;
//! - aterm-mesh-core's tests, which hold its kernels and folds to the islands' answer sheets.
//!
//! The crate is a default member, so a bare `targo --unverified build` checks them too.
//!
//! `islands.rs` lists them in their authored order. aterm-mesh-core mounts the same list on the
//! Trust lane (`cfg(trust_verify)`), where its contracts cite the spec kernels and its binding
//! islands follow them.
#![forbid(unsafe_code)]

#[cfg(clean_islands)]
mod islands;
