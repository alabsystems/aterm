// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// aterm-mesh-core's no-lost-edit spec island, which opens P1's island (`AtermMeshRevocation`).
// Whoever mounts this list mounts the temporal prelude and P1's island before it: this crate's
// lib.rs in every Trust build, aterm-mesh-core's lib.rs on the Trust lane. Each path is relative
// to this file's folder, whichever crate mounts it.

#[rustfmt::skip]
#[path = "../../aterm-mesh-core/src/no_lost_edit_island.rs"]
mod no_lost_edit;
