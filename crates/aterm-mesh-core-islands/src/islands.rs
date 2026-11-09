// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// aterm-mesh-core's spec islands, in their authored order: the temporal prelude every property's
// island opens (`AtermMeshTemporal`), P1's island and its Tier 2, which later islands open
// (`AtermMeshRevocation`), then each property's island. This crate's lib.rs mounts the list in
// every Trust build and aterm-mesh-core's lib.rs on the Trust lane; each path is relative to this
// file's folder, whichever crate mounts it.

#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/temporal_island.rs"]
mod temporal;

#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/revocation_island.rs"]
mod revocation;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/revocation_no_leak_island.rs"]
mod revocation_no_leak;

#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/capacity_island.rs"]
mod capacity;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/convergence_island.rs"]
mod convergence;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/gap_heal_island.rs"]
mod gap_heal;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/inbox_move_island.rs"]
mod inbox_move;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/no_lost_edit_island.rs"]
mod no_lost_edit;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/relay_island.rs"]
mod relay;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/relay_digest_island.rs"]
mod relay_digest;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/relay_live_island.rs"]
mod relay_live;
