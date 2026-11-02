// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ALAB LAB MODEL (docs/DESIGN-alab-lab-model-2026-11-01.md): the owner's machines,
//! accounts, addresses, mailboxes and seats as typed ATT/1 tables (`crate::typed`) on Commons,
//! which every Mac holds and repairs peer to peer, so no server, leader or poller is involved.
//!
//! - [`registry`]: the compiled schemas of `alab/registry`, the one catalog table that holds the
//!   owner's data. It takes a reserved catalog slot (`catalog::reserved`), so a new Mac can
//!   always store it however many declarations precede it.
//! - [`seeds`]: the provider, CLI and plan rows every build compiles in, written only into
//!   cells a complete scan found absent.
//! - [`Registry`] (Unix): declare, read, seed and publish the registry's schema rows.
//! - [`import`] (Unix): the owner's account sheets and the Macs this Mac knows of, planned as
//!   three-way registry writes; [`command`] runs `aterm db alab import|check|adopt`, and
//!   [`checks`] holds the lab's own rules.
//!
//! The journal ring's table (`catalog::ring::JOURNAL`) is the other home; its publisher is
//! later work (design §11 step 8).

pub mod checks;
#[cfg(unix)]
pub mod command;
#[cfg(unix)]
pub mod import;
pub mod registry;
pub mod seeds;
#[cfg(unix)]
mod store;
#[cfg(unix)]
pub use crate::att::View;
#[cfg(unix)]
pub use store::{Registry, Seeded};

#[cfg(all(test, unix))]
mod tests;
