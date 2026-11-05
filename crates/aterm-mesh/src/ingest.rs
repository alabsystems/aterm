// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ingest consumer (B7b, `docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, "Tables arrive
//! through one consumer"): one consumer group on `tables.log` that hands each record to its
//! store's own checks, a table operation to the table store's ingest and a membership record to
//! `aterm_secrets::membership_log::Store::ingest_records`. An operation whose writer is not yet a
//! seat in this Mac's view waits, by offset, and is tried again after each new membership record;
//! the group commits only past records that were applied, refused for good, or set waiting.
//!
//! Empty until B7b. It will read `tables.log` through [`crate::Mesh::read_client`].
