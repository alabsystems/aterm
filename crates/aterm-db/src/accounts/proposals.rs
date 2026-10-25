// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Account/contact proposals on the default Commons table. These records never
//! grant budget, machine-name, credential or provider-login authority. Network
//! exclusions pause replication; local reads and offline edits remain available.

use crate::{
    Error, Result,
    catalog::Descriptor,
    database::{Change, Command},
    table::{
        Scope, Session,
        store::{Receipt, Store},
    },
};
use astream_wire::sync::Digest;
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};

use crate::documents::log::Log;
pub use crate::documents::log::{Operation, Snapshot};
pub(crate) mod command;

const MAX_PAYLOAD: usize = 64 * 1024;

fn descriptor(bootstrap: &Bootstrap) -> Result<Descriptor> {
    Descriptor::application(bootstrap.trust(), "accounts", "proposals", 1)
}

/// An installation's account proposal database. Construction performs no I/O.
/// Each operation obtains a fresh locked membership observation and opens one
/// short-lived local store; no handle or secret is held across network I/O.
pub struct Proposals {
    paths: Paths,
}
impl Proposals {
    pub fn at(paths: Paths) -> Self {
        Self { paths }
    }
    /// Publish the deterministic table once. Independent offline opens coalesce.
    pub fn initialize(&self) -> Result<Scope> {
        self.with_store(true, |_, _, scope| Ok(scope))
    }
    /// Read current eligible metadata and signed/local provenance. A missing or
    /// damaged established store is never silently replaced.
    pub fn snapshot(&self) -> Result<Snapshot> {
        self.with_store(false, |session, store, scope| {
            Ok(Log::read_with(session, store, scope, decode)?.snapshot())
        })
    }
    /// Diagnose explicit references without resolving credentials or authorizing
    /// login. This reads the existing table and appends no application operation.
    pub fn check(&self) -> Result<aterm_json::Value> {
        Ok(super::references::inspect(&self.snapshot()?))
    }
    /// Commit one whole-value transaction. Requests are scoped to this certified
    /// writer incarnation. Exact retries return their original durable receipt.
    /// `None` deletes a value reversibly; this API creates no permanent stop.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbProposalRetry",
            action = "Write",
            project = "aterm_db::accounts::proposals::tests::retry_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbProposalRetry",
            action = "Retry",
            project = "aterm_db::accounts::proposals::tests::retry_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbProposalRetry",
            action = "Change",
            project = "aterm_db::accounts::proposals::tests::retry_transition"
        )
    )]
    pub fn write(&self, request: [u8; 16], changes: Vec<Change>) -> Result<Receipt> {
        let command = Command::Write { request, changes };
        let payload = encode(&command)?;
        self.with_store(true, |session, store, scope| {
            session.writer()?;
            let log = Log::read_with(session, store, scope, decode)?;
            log.writable(session.actor())?;
            if let Some(id) = log.retry(session.actor(), &command)? {
                return receipt(session, store, &id);
            }
            store.commit_catalog(session, &payload)
        })
    }
    /// Append an exact compensating edit while every affected key still has the
    /// target as its sole candidate. Later-arriving offline edits remain siblings.
    /// Deletion is reversible metadata; authority/resource stops cannot be made here.
    pub fn revert(&self, request: [u8; 16], operation: Digest) -> Result<Receipt> {
        self.with_store(false, |session, store, scope| {
            session.writer()?;
            let log = Log::read_with(session, store, scope, decode)?;
            log.writable(session.actor())?;
            if let Some(id) = log.revert_retry(session.actor(), request, operation)? {
                return receipt(session, store, &id);
            }
            let command = log.compensate(request, operation)?;
            store.commit_catalog(session, &encode(&command)?)
        })
    }
    fn with_store<T>(
        &self,
        create: bool,
        action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
    ) -> Result<T> {
        crate::documents::application::with_store(&self.paths, create, descriptor, action)
    }
}

fn receipt(session: &Session<'_>, store: &Store, id: &Digest) -> Result<Receipt> {
    store
        .receipt(session, id)?
        .ok_or_else(|| Error::new("proposal receipt is unavailable"))
}

fn encode(command: &Command) -> Result<Vec<u8>> {
    let payload = command.encode()?;
    if decode(&payload)? != *command {
        return Err(Error::new("proposal encoding would change its meaning"));
    }
    Ok(payload)
}

fn decode(payload: &[u8]) -> Result<Command> {
    if payload.len() > MAX_PAYLOAD {
        return Err(Error::new("account proposal exceeds payload limit"));
    }
    let command = Command::decode(payload)?;
    if command.encode()? != payload {
        return Err(Error::new("account proposal encoding is not canonical"));
    }
    let changes = command
        .changes()
        .ok_or_else(|| Error::new("account proposals cannot authorize resources or membership"))?;
    for change in changes {
        match change.collection.as_str() {
            "accounts" | "mailboxes" | "email-addresses" | "phones" => {}
            "machine-proposals" => {
                if let Some(value) = &change.value {
                    crate::machines::validate(&change.key, value)?;
                }
            }
            _ => return Err(Error::new("unsupported account proposal collection")),
        }
    }
    Ok(command)
}

#[cfg(test)]
mod tests;
