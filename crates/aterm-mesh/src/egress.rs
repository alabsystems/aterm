// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The egress (B7b, `docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, "Tables arrive through one
//! consumer"): every operation that enters a store from this Mac, a local write or a record an
//! older peer delivered by an old path, is published to `tables.log` under this Mac's owner
//! segment, and at each start the node service publishes any it has not yet published. A record
//! that came in through the relay or `Inventory` is never published again by this Mac.
//!
//! Empty until B7b. It will publish through [`crate::Mesh::publish`], which signs each record
//! and numbers it under the log's incarnation lease ([`crate::relay::Origin`]); the `MeshRelay`
//! model's `Publish` action (§9) anchors here.
