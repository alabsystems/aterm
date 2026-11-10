// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Account/contact proposals on the default Commons table. These records never
//! grant budget, machine-name, credential or provider-login authority. Network
//! exclusions pause replication; local reads and offline edits remain available.

use super::publication::{Completion, Condition, Destination, Pending};
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
use std::collections::BTreeSet;

pub use crate::documents::log::{Operation, Snapshot};
use crate::documents::{application, log::Log};
pub(crate) mod command;

const MAX_PAYLOAD: usize = 64 * 1024;

#[cfg(test)]
thread_local! {
    static CHAIN_MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

fn descriptor(bootstrap: &Bootstrap) -> Result<Descriptor> {
    Descriptor::application(bootstrap.trust(), "accounts", "proposals", 1)
}

/// An installation's account proposal database. Construction performs no I/O.
/// Mutations obtain a fresh locked membership observation; metadata snapshots
/// read a verified committed prefix beside writers. No handle or secret is held across network I/O.
pub struct Proposals {
    paths: Paths,
    deadline: Option<std::time::Instant>,
}
impl Proposals {
    pub fn at(paths: Paths) -> Self {
        Self {
            paths,
            deadline: None,
        }
    }
    /// Keep every nested observation inside one caller-owned pass budget.
    pub fn at_until(paths: Paths, deadline: std::time::Instant) -> Self {
        Self {
            paths,
            deadline: Some(deadline),
        }
    }
    /// Publish the deterministic table once. Independent offline opens coalesce.
    pub fn initialize(&self) -> Result<Scope> {
        self.with_store(true, |_, _, scope| Ok(scope))
    }
    /// Read current eligible metadata and signed/local provenance. A missing or
    /// damaged established store is never silently replaced.
    pub fn snapshot(&self) -> Result<Snapshot> {
        let read =
            |session: &Session<'_>, store: &Store, _| Self::snapshot_from_store(session, store);
        match self.deadline {
            Some(deadline) => crate::documents::application::observe_store_until(
                &self.paths,
                descriptor,
                deadline,
                read,
            ),
            None => crate::documents::application::observe_store(&self.paths, descriptor, read),
        }
    }

    /// Capture the identity of an already declared proposal table and this
    /// writer incarnation. This observation creates no publication authority.
    pub fn publication_destination(&self) -> Result<Destination> {
        let read =
            |session: &Session<'_>, _: &Store, scope| Destination::observed(scope, session.actor());
        match self.deadline {
            Some(deadline) => {
                application::observe_store_until(&self.paths, descriptor, deadline, read)
            }
            None => application::observe_store(&self.paths, descriptor, read),
        }
    }

    /// Deliver the exact retained description command. A changed destination
    /// or writer refuses before opening the table for mutation. Exact retries
    /// use the original application receipt, including after a lost reply.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbPublicationChain",
            action = "Scenario",
            reason = "The environment selects retained, missing, conflicting or superseded history."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbPublicationChain",
            action = "Check",
            project = "aterm_db::accounts::proposals::chain_tests::transition"
        )
    )]
    pub fn deliver_publication(&self, pending: &Pending) -> Result<Completion> {
        let command = pending.validate()?;
        let deadline = self.deadline.unwrap_or_else(crate::admission::deadline);
        let receipt = application::with_store_guarded_until(
            &self.paths,
            false,
            descriptor,
            deadline,
            |session, _, descriptor| {
                if !pending
                    .destination()
                    .matches(descriptor.scope, session.actor())?
                {
                    return Err(Error::new("publication destination or writer changed"));
                }
                Ok(())
            },
            |session, store, scope| {
                conditional_write(session, store, scope, &command, |log, change| {
                    publication_expected(log, session.actor(), pending.condition(), change)
                })
            },
        )?;
        Ok(Completion::admitted(pending, receipt.id))
    }
    /// [`Self::snapshot`] where the proposal table is declared here: `None` where nothing declared
    /// it, this Mac having set up no account catalog, and every other failure remains an error.
    pub fn snapshot_if_declared(&self) -> Result<Option<Snapshot>> {
        crate::documents::application::observe_store_if_here(
            &self.paths,
            descriptor,
            self.deadline,
            |session: &Session<'_>, store: &Store, _| Self::snapshot_from_store(session, store),
        )
    }
    /// Project an already opened proposal store without reopening or enrolling
    /// its catalog. The caller keeps the authenticated session alive.
    pub(crate) fn snapshot_from_store(session: &Session<'_>, store: &Store) -> Result<Snapshot> {
        Ok(Log::read_with(session, store, store.scope(), decode)?.snapshot())
    }
    pub(crate) fn snapshot_in_session(
        session: &Session<'_>,
        deadline: std::time::Instant,
    ) -> Result<Snapshot> {
        application::observe_store_in_session(
            session,
            descriptor(session.control_context()?.0)?,
            deadline,
            |session, store, _| Self::snapshot_from_store(session, store),
        )
    }

    /// Diagnose explicit references without resolving credentials or authorizing
    /// login. This reads the existing table and appends no application operation.
    pub fn check(&self) -> Result<aterm_json::Value> {
        Ok(super::references::inspect(&self.snapshot()?))
    }
    /// Describe one observed account and its complete explicit contact closure.
    /// Digests identify a proposal for review, never permission to use a login.
    /// No credentials are read and no application operation is appended.
    pub fn plan(&self, account: &str) -> Result<aterm_json::Value> {
        super::plans::inspect(&self.snapshot()?, account)
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
            application::append(session, store, &payload)
        })
    }

    /// Replace one ordinary metadata value only while its sole observed
    /// candidate is unchanged and retained history is complete. This confers no
    /// account or credential authority.
    /// Exact retries return the first receipt even after that write supersedes
    /// the expected candidate; unrelated later edits are never overwritten.
    pub fn replace_if_current(
        &self,
        request: [u8; 16],
        collection: &str,
        key: &str,
        expected: Digest,
        value: aterm_json::Value,
    ) -> Result<Receipt> {
        self.write_if_current(request, collection, key, Some(expected), value)
    }
    /// Create an ordinary metadata row only if its key has never been occupied.
    /// Exact retries return their first receipt; conflicting rows and tombstones
    /// cannot be overwritten by an import or credential intake. New writes need
    /// complete retained history.
    pub fn create_if_absent(
        &self,
        request: [u8; 16],
        collection: &str,
        key: &str,
        value: aterm_json::Value,
    ) -> Result<Receipt> {
        self.write_if_current(request, collection, key, None, value)
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbProposalConditional",
            action = "FreshCreate",
            project = "aterm_db::accounts::proposals::conditional_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbProposalConditional",
            action = "FreshReplace",
            project = "aterm_db::accounts::proposals::conditional_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbProposalConditional",
            action = "GapCreate",
            project = "aterm_db::accounts::proposals::conditional_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbProposalConditional",
            action = "GapReplace",
            project = "aterm_db::accounts::proposals::conditional_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbProposalConditional",
            action = "RetryGap",
            project = "aterm_db::accounts::proposals::conditional_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbProposalConditional",
            action = "HealCreate",
            project = "aterm_db::accounts::proposals::conditional_tests::transition"
        )
    )]
    fn write_if_current(
        &self,
        request: [u8; 16],
        collection: &str,
        key: &str,
        expected: Option<Digest>,
        value: aterm_json::Value,
    ) -> Result<Receipt> {
        let command = Command::Write {
            request,
            changes: vec![Change {
                collection: collection.into(),
                key: key.into(),
                value: Some(value),
            }],
        };
        self.with_store(false, |session, store, scope| {
            conditional_write(session, store, scope, &command, |_, _| Ok(expected))
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
            let command = log.compensate(request, operation, &BTreeSet::new())?;
            application::append(session, store, &encode(&command)?)
        })
    }
    fn with_store<T>(
        &self,
        create: bool,
        action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
    ) -> Result<T> {
        match self.deadline {
            Some(deadline) => crate::documents::application::with_store_until(
                &self.paths,
                create,
                descriptor,
                deadline,
                action,
            ),
            None => {
                crate::documents::application::with_store(&self.paths, create, descriptor, action)
            }
        }
    }
}

fn conditional_write(
    session: &Session<'_>,
    store: &mut Store,
    scope: Scope,
    command: &Command,
    expected: impl FnOnce(&Log, &Change) -> Result<Option<Digest>>,
) -> Result<Receipt> {
    let Command::Write { changes, .. } = command else {
        return Err(Error::new("conditional proposal needs a write"));
    };
    let [change] = changes.as_slice() else {
        return Err(Error::new("conditional proposal needs one description"));
    };
    let payload = encode(command)?;
    session.writer()?;
    let log = Log::read_with(session, store, scope, decode)?;
    #[cfg(test)]
    let retry_first = CHAIN_MUTANT.get() != 3;
    #[cfg(not(test))]
    let retry_first = true;
    if retry_first && let Some(id) = log.retry(session.actor(), command)? {
        return receipt(session, store, &id);
    }
    log.writable(session.actor())?;
    if !log.history_complete() {
        return Err(Error::new(
            "conditional proposal write needs complete retained history",
        ));
    }
    let expected = expected(&log, change)?;
    let cell = log.project_cell(&change.collection, &change.key);
    let matches = match (cell.as_ref(), expected) {
        (None, None) => true,
        (Some(cell), Some(expected)) => {
            !cell.stopped && cell.candidates.len() == 1 && cell.candidates[0].operation == expected
        }
        _ => false,
    };
    #[cfg(test)]
    let matches = matches || CHAIN_MUTANT.get() == 1;
    if !matches {
        return Err(Error::new("metadata candidate changed before replacement"));
    }
    #[cfg(test)]
    if !retry_first && let Some(id) = log.retry(session.actor(), command)? {
        return receipt(session, store, &id);
    }
    application::append(session, store, &payload)
}

fn publication_expected(
    log: &Log,
    actor: astream_wire::sync::Actor,
    condition: &Condition,
    change: &Change,
) -> Result<Option<Digest>> {
    use crate::database::decode_hex;
    match condition {
        Condition::NeverUsed => Ok(None),
        Condition::Existing { operation } => Ok(Some(decode_hex(operation)?)),
        Condition::After {
            request,
            command_sha256,
        } => {
            let expected: Digest = decode_hex(command_sha256)?;
            let Some((id, predecessor)) =
                log.resolve_request(actor, decode_hex(request)?, |old| {
                    let matches = aterm_digest::Sha256::digest(&old.encode()?) == expected;
                    #[cfg(test)]
                    let matches = matches || CHAIN_MUTANT.get() == 2;
                    Ok(matches)
                })?
            else {
                return Err(Error::new("publication predecessor is not retained"));
            };
            let Command::Write { changes, .. } = predecessor else {
                return Err(Error::new("publication predecessor is not a put"));
            };
            let [previous] = changes.as_slice() else {
                return Err(Error::new("publication predecessor needs one description"));
            };
            if previous.value.is_none()
                || previous.collection != change.collection
                || previous.key != change.key
            {
                return Err(Error::new(
                    "publication predecessor names another description",
                ));
            }
            Ok(Some(id))
        }
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

fn validate_collection(collection: &str) -> Result<()> {
    match collection {
        "accounts" | "mailboxes" | "email-addresses" | "phones" | "machine-proposals" => Ok(()),
        _ => Err(Error::new("unsupported account proposal collection")),
    }
}

fn decode(payload: &[u8]) -> Result<Command> {
    if payload.len() > MAX_PAYLOAD {
        return Err(Error::new("account proposal exceeds payload limit"));
    }
    let command = Command::decode(payload)?;
    if command.encode()? != payload {
        return Err(Error::new("account proposal encoding is not canonical"));
    }
    if matches!(command, Command::Merge { .. }) {
        return Ok(command);
    }
    let changes = command
        .changes()
        .ok_or_else(|| Error::new("account proposals cannot authorize resources or membership"))?;
    for change in changes {
        validate_collection(&change.collection)?;
        if change.collection == "machine-proposals"
            && let Some(value) = &change.value
        {
            crate::machines::validate(&change.key, value)?;
        }
    }
    Ok(command)
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod frontier_tests;

#[cfg(test)]
#[path = "proposals/conditional_tests.rs"]
mod conditional_tests;

#[cfg(test)]
#[path = "proposals/chain_tests.rs"]
mod chain_tests;
