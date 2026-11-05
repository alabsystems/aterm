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
//! Every module the mesh will have is declared here. Four start empty and are filled by later
//! steps: [`egress`] and [`ingest`] (B7b), [`inventory`] (B7b) and [`presence`] (B7c).
//!
//! Nothing here runs unless the node service is built with `aterm_types::mesh_switch::MESH` on:
//! the node service opens a [`Mesh`] only then.

#![forbid(unsafe_code)]
#![cfg(unix)]

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

pub mod egress;
pub mod ingest;
pub mod inventory;
pub mod logs;
pub mod overlay;
pub mod presence;
pub mod relay;

pub use logs::{Mesh, MeshConfig, MeshLog, Standings};
pub use overlay::Attached;
pub use relay::{Event, Published};

#[cfg(test)]
mod tests;
