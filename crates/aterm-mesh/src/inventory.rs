// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `Inventory` exchange (B7b, `docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, "No anti-entropy
//! timer"): the `astream_wire::sync` digest exchange with one neighbour that fills what the relay
//! cannot, started only by a gap notice (a relay's cursor fell below what its log keeps), a lineage
//! change (a log with a new identity), or a new Mac whose stores are empty. After an exchange that
//! moved nothing, the pair does not exchange again until a digest changes.
//!
//! Empty until B7b. The node service answers an `Inventory` stream "unserved" until then; the
//! `MeshRelay` model's `Inventory` action (§9) anchors here.
