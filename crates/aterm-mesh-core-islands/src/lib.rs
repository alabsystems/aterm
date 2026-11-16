// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The spec islands of `aterm-mesh-core` (`docs/DESIGN-one-mesh-proofs-2026-11-05.md`) that open
//! at most the temporal prelude, kernel-checked by every Trust build of this crate
//! (`cfg(clean_islands)`).
//!
//! The islands prove the laws of aterm-mesh-core's kernels and name no Rust, so nothing ties their
//! check to the kernels' compile. Trust checks a crate's islands before it writes the crate's
//! metadata, which every dependent waits for: in aterm-mesh-core they held aterm-link, aterm-mesh,
//! aterm-agent, aterm-gui and the `aterm` link behind three minutes of kernel work
//! (`docs/measured/mesh-core-islands-off-the-chain-2026-11-08.md`). Here only what needs their
//! verdict waits: aterm-mesh-core's tests, which hold its kernels and folds to the islands' answer
//! sheets. Their verdict depends only on the island text and the pinned compiler, so a landing
//! checks them where it can change: an island edit selects aterm-mesh-core, whose tests link these
//! crates, and a toolchain or cargo-config change replans every crate. The product does not link
//! them, so a fresh checkout's build of `aterm` does not prove the same theorems again.
//!
//! Trust checks a crate's islands on one core, so they are split three ways, each about a minute
//! and a half of kernel work or less, which run beside one another: this crate;
//! `aterm-mesh-core-islands-p1`, P1's island and the islands that open it; and
//! `aterm-mesh-core-islands-no-lost-edit`, the largest, which opens P1 too. Each mounts the
//! preludes its islands open, then its `islands.rs`, which lists each of its islands once.
//! aterm-mesh-core mounts the preludes once and then the three lists on the Trust lane
//! (`cfg(trust_verify)`), where its contracts cite the spec kernels and its binding islands follow
//! them. None is a default member: a bare `targo --unverified build` leaves them to the checks
//! above.
#![forbid(unsafe_code)]

#[cfg(clean_islands)]
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/temporal_island.rs"]
mod temporal;

#[cfg(clean_islands)]
mod islands;
