// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ALAB LAB MODEL (docs/DESIGN-alab-lab-model-2026-11-01.md): the owner's machines,
//! accounts, addresses, mailboxes and the accounts each machine prefers as typed ATT/1 tables (`crate::typed`) on Commons,
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
//! - [`journal`] (Unix): the journal ring's table (`catalog::ring::JOURNAL`), the other home:
//!   what each Mac sees while it uses an account, noted by the harness into a local spool and
//!   published by the Commons owner's flusher (`upkeep`) in its fixed share of each week.
//! - [`mail`]: where an address's codes arrive, following an alias to the address it is linked
//!   with.
//! - [`register`]: what a Mac states about itself, and the adoption rule that binds its Commons
//!   enrollment to a machine row.
//! - [`view`]: LabView, what the harness reads of the registry: this Mac's preferences, the Macs
//!   preferring an account and where an address's codes arrive, projected once and cached.
//! - [`discover`]: what a Mac records of a Proton mailbox's address list while the mailbox is open
//!   to read a sign-in code.

pub mod checks;
#[cfg(unix)]
pub mod command;
pub mod discover;
#[cfg(unix)]
pub mod import;
#[cfg(unix)]
pub mod journal;
pub mod mail;
pub mod register;
pub mod registry;
#[cfg(unix)]
pub mod roll;
pub mod seeds;
#[cfg(unix)]
pub(super) mod store;
pub mod view;
#[cfg(unix)]
pub use crate::att::View;
#[cfg(unix)]
pub use roll::Rolled;
#[cfg(unix)]
pub use store::{Discovered, Inventory, LabRead, Registry, Seeded};
#[cfg(unix)]
pub(crate) mod upkeep;

#[cfg(all(test, unix))]
mod tests;
