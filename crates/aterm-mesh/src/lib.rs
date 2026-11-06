// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ALab's mesh logs and their relay (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, §11 B7a).
//!
//! The node service hosts two astream logs in `<state_dir>/mesh/`: `tables.log` (the membership
//! log's records and every lab table's operations) and `metrics.log` (each Mac's own rows). Each
//! is sealed at rest under a `rest.key` beside it, guarded by a secret of its own, and served on a
//! local socket. A record this Mac originates is signed by its node key ([`relay::Origin`]) and
//! numbered in astream's incarnation layout, so a sequence number never repeats; it then travels
//! to every Mac over the neighbour links (`aterm_net::fleet::link`), one push relay per neighbour
//! per log ([`relay`]), and each broker appends it once, from whichever neighbour delivers it
//! first. The overlay ([`overlay`]) keeps the neighbours; the membership log decides who is an
//! origin and whose streams are refused ([`logs::Standings`]).
//!
//! Tables move over the relay (B7b): the egress ([`egress`]) publishes every record that enters a
//! store from this Mac, the ingest consumer ([`ingest`]) hands each record another Mac originated
//! to its store's own checks, and the `Inventory` exchange ([`inventory`]) fills what the relay
//! cannot, each over the stores the node service hands it ([`stores`]).
//! [`presence`] supplies the origin-signed presence rows and their per-seat reading (B7c).
//! [`mail`] runs the fabric's mail pushes over the same links, one hop to the addressed Mac (C3).
//! [`push`] answers the claims push a window or a session lane asks of the node service (B7b).
//!
//! Nothing here runs unless the node service is built with `aterm_types::mesh_switch::MESH` on:
//! the node service opens a [`Mesh`] only then.

#![forbid(unsafe_code)]
#![cfg(unix)]

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

pub mod egress;
mod flow;
pub mod ingest;
pub mod inventory;
pub mod logs;
pub mod mail;
pub mod overlay;
pub mod presence;
pub mod push;
pub mod relay;
pub mod stores;

pub use flow::{Pushed, current};
pub use logs::{Mesh, MeshConfig, MeshLog, Standings};
pub use overlay::Attached;
pub use relay::{Event, Published};
pub use stores::Stores;

#[cfg(test)]
mod tests;
