// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// aterm-mesh-core's spec islands that open at most the temporal prelude (`AtermMeshTemporal`),
// which whoever mounts this list mounts before it. This crate's lib.rs mounts the list in every
// Trust build and aterm-mesh-core's lib.rs on the Trust lane; each path is relative to this
// file's folder, whichever crate mounts it.

#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/capacity_island.rs"]
mod capacity;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/inbox_move_island.rs"]
mod inbox_move;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/relay_digest_island.rs"]
mod relay_digest;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/relay_live_island.rs"]
mod relay_live;
