// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P1's Tier 2 and the spec islands of aterm-mesh-core that open P1's island
// (`AtermMeshRevocation`), but no_lost_edit's, which `aterm-mesh-core-islands-no-lost-edit` holds.
// Whoever mounts this list mounts the temporal prelude and P1's island before it: this crate's
// lib.rs in every Trust build, aterm-mesh-core's lib.rs on the Trust lane. Each path is relative
// to this file's folder, whichever crate mounts it.

#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/revocation_no_leak_island.rs"]
mod revocation_no_leak;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/convergence_island.rs"]
mod convergence;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/gap_heal_island.rs"]
mod gap_heal;
#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/relay_island.rs"]
mod relay;
