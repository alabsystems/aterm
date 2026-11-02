// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `alab/registry` on Commons: declared in its reserved catalog slot (`catalog::reserved`),
//! read as ATT/1 with the per-Mac quota applied, seeded after a complete scan, and given its
//! schema rows by an owner verb only.

use super::{
    registry::{APPLICATION, REGISTRY, ROLE, VERSION},
    seeds,
};
use crate::{
    Error, Result,
    att::{View, decode, pack},
    catalog::Descriptor,
    database::Command,
    documents::{application, log::Log},
    table::Scope,
    typed,
};
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};

fn descriptor(bootstrap: &Bootstrap) -> Result<Descriptor> {
    Descriptor::application(bootstrap.trust(), APPLICATION, ROLE, VERSION)
}

/// What a seed pass wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seeded {
    /// Records appended now.
    pub records: usize,
    /// Records an earlier pass of this Mac already wrote with the same content.
    pub retried: usize,
}

/// `alab/registry` on this Mac's Commons.
pub struct Registry {
    paths: Paths,
}

impl Registry {
    pub fn at(paths: Paths) -> Self {
        Self { paths }
    }

    /// Declare the registry, whose catalog slot is reserved, and enroll it under this Mac's
    /// writer. Reopening coalesces with an existing declaration.
    ///
    /// # Errors
    /// No Commons writer, no storage room at all, or an incompatible declaration.
    pub fn open(&self) -> Result<Scope> {
        application::with_store(&self.paths, true, descriptor, |_, _, scope| Ok(scope))
    }

    /// Read the registry this Mac holds, without enrolling it.
    ///
    /// # Errors
    /// No Commons, the registry is not declared or not subscribed here, or its storage is
    /// damaged.
    pub fn view(&self) -> Result<View> {
        crate::att::Handle::at(self.paths.clone(), &crate::att::REGISTRY).view()
    }

    /// Write the compiled provider, CLI and plan rows ([`seeds`]) into every cell a complete
    /// scan of the registry finds absent, under this Mac's quota of automatic records: each
    /// row in one record, each record with a content-derived request id, so a re-run writes
    /// nothing and a retry returns its first receipt.
    ///
    /// # Errors
    /// No Commons writer; this Mac's history of the registry is incomplete (withheld,
    /// unavailable or refused operations), so a scan could miss a held cell; or this Mac's
    /// automatic records are spent.
    pub fn seed(&self) -> Result<Seeded> {
        application::with_store(&self.paths, true, descriptor, |session, store, scope| {
            session.writer()?;
            let mut seeded = Seeded::default();
            let mut log = Log::read_with(session, store, scope, decode)?;
            log.writable(session.actor())?;
            let view = View::read(&REGISTRY, session, &log);
            if !view.complete {
                return Err(Error::new(
                    "the registry's history here is incomplete; seeds wait for a complete scan",
                ));
            }
            let mut spent = view.quota.spent.get(&session.actor()).copied().unwrap_or(0);
            for changes in pack(seeds::rows_for(&view.cells)?)? {
                let request = typed::request_id(APPLICATION, ROLE, &changes)?;
                let command = Command::Write { request, changes };
                if log.retry(session.actor(), &command)?.is_some() {
                    seeded.retried += 1;
                    continue;
                }
                if spent >= REGISTRY.self_quota {
                    return Err(Error::new(
                        "this Mac's automatic records in the registry are spent",
                    ));
                }
                application::append(session, store, &command.encode()?)?;
                spent += 1;
                seeded.records += 1;
                // Re-read before admitting another append (`Log::append_records_bound`).
                log = Log::read_with(session, store, scope, decode)?;
            }
            Ok(seeded)
        })
    }

    /// Publish the compiled schema rows (design §3.5), an owner verb: only where the stored
    /// row is absent or below the compiled version (`att::Handle::publish_schema`).
    ///
    /// # Errors
    /// No Commons writer, or the registry cannot be read or written.
    pub fn publish_schema(&self) -> Result<usize> {
        crate::att::Handle::at(self.paths.clone(), &crate::att::REGISTRY).publish_schema()
    }

    /// Append `records` as this Mac, one record each (tests drive owner edits, births and
    /// owned rows with it).
    #[cfg(test)]
    pub(crate) fn write(&self, records: Vec<Vec<crate::database::Change>>) -> Result<()> {
        application::with_store(&self.paths, true, descriptor, |session, store, _| {
            session.writer()?;
            for changes in records {
                let command = Command::Write {
                    request: crate::crypto::random()?,
                    changes,
                };
                application::append(session, store, &command.encode()?)?;
            }
            Ok(())
        })
    }
}
