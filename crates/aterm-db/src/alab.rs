// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ALAB LAB MODEL (docs/DESIGN-alab-lab-model-2026-11-01.md): the owner's machines,
//! accounts, addresses, mailboxes and the accounts each machine prefers as typed ATT/1 tables (`crate::typed`) on Commons,
//! which every Mac holds and repairs peer to peer, so no server, leader or poller is involved.
//!
//! - [`registry`]: the compiled schemas of `alab/registry`, the one catalog table that holds the
//!   owner's data. It takes a reserved catalog slot (`catalog::reserved`), so a new Mac can
//!   always store it however many declarations precede it.
//! - [`seeds`]: the provider, CLI and plan rows this build compiles in, written only into
//!   cells a complete scan found absent.
//! - [`Registry`] (Unix): declare, read, seed and publish the registry's schema rows.
//! - [`history`] (Unix): what a roll carries of a retired generation's history, a secret's
//!   value withheld, and what a Mac's release of that generation waits for.
//! - [`import`] (Unix): the owner's account sheets and the Macs this Mac knows of, planned as
//!   three-way registry writes; [`command`] runs `aterm db alab import|check|adopt`, and
//!   [`checks`] holds the lab's own rules.
//! - [`journal`] (Unix): the journal ring's table (`catalog::ring::JOURNAL`), the other home:
//!   what each Mac sees while it uses an account, noted by the harness into a local spool and
//!   published by the Commons owner's flusher (`upkeep`) in its fixed share of each week.
//! - [`mail`]: where an address's codes arrive, following an alias to the address it is linked
//!   with.
//! - [`merge`]: duplicate machine rows folded into the one row their Mac is bound to, with
//!   `merged_into`.
//! - [`register`]: what a Mac states about itself, and the adoption rule that binds its Commons
//!   enrollment to a machine row.
//! - [`view`]: LabView, what the harness reads of the registry: this Mac's preferences, the Macs
//!   preferring an account and where an address's codes arrive, projected once and cached.
//! - [`account_names`]: each account's slug and name, derived in one place, and the account a
//!   word names, which every account command resolves the same way.
//! - [`discover`]: what a Mac records of a Proton mailbox's address list while the mailbox is open
//!   to read a sign-in code.
//! - [`sighted`]: where and when each account was sighted, one fold of every Mac's `account-obs`
//!   rows and the journal's `seen` events for every reader that says so.

pub mod account_names;
pub mod account_status;
pub mod checks;
#[cfg(unix)]
pub mod command;
pub mod discover;
#[cfg(unix)]
pub mod history;
#[cfg(unix)]
pub mod import;
#[cfg(unix)]
pub mod journal;
pub mod link;
pub mod mail;
pub mod merge;
pub mod observe;
pub mod register;
pub mod registry;
#[cfg(unix)]
pub mod roll;
pub mod secret;
pub mod seeds;
pub mod sighted;
#[cfg(unix)]
pub(super) mod store;
#[cfg(unix)]
pub mod vault;
pub mod view;
pub use crate::typed::View;
#[cfg(unix)]
pub use roll::Rolled;
#[cfg(all(unix, any(test, debug_assertions)))]
pub use store::add_admission_counts_for_test;
#[cfg(unix)]
pub use store::{
    AdmissionCounts, CatalogAbsenceRead, CatalogAbsenceRevision, Discovered, Inventory, LabRead,
    LocalSourceRead, LocalSourceRevision, Observed, ProjectionRead, ProjectionRevision, Registry,
    Seeded, SourceRead, SourceRevision, admission_counts,
};
#[cfg(unix)]
pub(crate) mod upkeep;

#[cfg(all(test, unix))]
mod tests;
