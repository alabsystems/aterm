// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{
    Error, Result,
    database::{Cell, Change, Command},
    documents::Cells,
    table::{
        Scope, Session,
        store::{Receipt, Store},
    },
};
use astream_wire::sync::{Actor, Digest, Header, Index};
use std::collections::{BTreeMap, BTreeSet};

/// Provenance is evidence, not authority. Authored time is a signed clock claim;
/// receipt time and offset describe only this replica's first durable retention.
#[derive(Clone, Debug)]
pub struct Operation {
    pub header: Header,
    pub roster: Digest,
    pub epoch: u64,
    pub authored_ms: u64,
    pub receipt: Receipt,
    pub request: Option<[u8; 16]>,
    pub reverts: Option<Digest>,
    /// Payload is currently decryptable under retained authenticated history.
    pub available: bool,
    /// Current membership and causal permission, separate from retained evidence.
    pub catalog_eligible: bool,
    /// Eligible and valid in this snapshot, including superseded values.
    pub eligible: bool,
    pub duplicate_of: Option<Digest>,
    pub refusal: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub scope: Scope,
    /// Includes sources retained through negative certificates and operations
    /// withheld by membership, forks or dependencies.
    pub retained: usize,
    /// Withheld by table membership or causal policy; application refusals are
    /// reported separately on each operation.
    pub withheld: usize,
    pub cells: BTreeMap<(String, String), Cell>,
    pub operations: Vec<Operation>,
}

pub(crate) struct Log {
    scope: Scope,
    retained: usize,
    index: Index,
    records: BTreeMap<Digest, (Operation, Option<Command>)>,
    conflicts: BTreeSet<Digest>,
    opaque_authors: BTreeSet<Actor>,
}
impl Log {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbProposalEvidence",
            action = "Withdraw",
            project = "aterm_db::accounts::proposals::tests::evidence_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbProposalEvidence",
            action = "Quarantine",
            project = "aterm_db::accounts::proposals::tests::evidence_transition"
        )
    )]
    pub(crate) fn read_with(
        session: &Session<'_>,
        store: &Store,
        scope: Scope,
        decode: fn(&[u8]) -> Result<Command>,
    ) -> Result<Self> {
        let mut log = Self {
            scope,
            retained: 0,
            index: Index::new(crate::database::MAX_RECORDS * 2),
            records: BTreeMap::new(),
            conflicts: BTreeSet::new(),
            opaque_authors: BTreeSet::new(),
        };
        for evidence in store.metadata_evidence(session)? {
            let header = evidence.header;
            let id = header.id;
            log.index
                .insert(header.clone())
                .map_err(|_| Error::new("invalid proposal causal history"))?;
            let available = evidence.record.is_some();
            let (command, refusal) = match &evidence.record {
                Some(record) => match decode(record.payload()) {
                    Ok(command) => (Some(command), None),
                    Err(error) => (None, Some(error.0)),
                },
                None => {
                    log.opaque_authors.insert(header.actor);
                    (None, None)
                }
            };
            let operation = Operation {
                header,
                roster: evidence.roster,
                epoch: evidence.epoch,
                authored_ms: evidence.authored_ms,
                receipt: evidence.receipt,
                request: command.as_ref().map(Command::request),
                reverts: match &command {
                    Some(Command::Revert { operation, .. }) => Some(*operation),
                    _ => None,
                },
                available,
                catalog_eligible: evidence.catalog_eligible,
                eligible: false,
                duplicate_of: None,
                refusal,
            };
            log.records.insert(id, (operation, command));
        }
        let mut requests = BTreeMap::<(Actor, [u8; 16]), Vec<Digest>>::new();
        for (id, (operation, command)) in &log.records {
            if let Some(command) = command {
                requests
                    .entry((operation.header.actor, command.request()))
                    .or_default()
                    .push(*id);
            }
        }
        for ids in requests.values_mut() {
            ids.sort_by_key(|id| (log.records[id].0.header.sequence, *id));
            let first = ids[0];
            if ids
                .iter()
                .any(|id| log.records[id].1 != log.records[&first].1)
            {
                for id in ids.iter() {
                    log.conflicts.insert(*id);
                    log.records.get_mut(id).unwrap().0.refusal =
                        Some("request identity reused with different commands".into());
                }
            } else {
                for id in ids.iter().skip(1) {
                    log.records.get_mut(id).unwrap().0.duplicate_of = Some(first);
                }
            }
        }
        log.retained = log.records.len();
        Ok(log)
    }
    fn project(
        &self,
        before: Option<Digest>,
    ) -> (Cells, BTreeMap<Digest, String>, BTreeSet<Digest>) {
        let mut cells = Cells::new();
        let mut refusals = BTreeMap::new();
        let mut eligible = BTreeSet::new();
        let mut structural = BTreeSet::new();
        let withheld = self
            .records
            .iter()
            .filter_map(|(id, (op, _))| (!op.catalog_eligible).then_some(*id))
            .collect();
        // Request conflicts are inert application data, not a signed-chain fork.
        // Keep their headers as dependencies so one author cannot suppress the
        // well-formed later edits of every peer that observed the bad request.
        for id in self.index.project_excluding(&withheld).ordered {
            let (operation, Some(command)) = &self.records[&id] else {
                continue;
            };
            if let Command::Revert {
                operation: target,
                changes,
                ..
            } = command
            {
                let target_changes = self
                    .records
                    .get(target)
                    .and_then(|(_, command)| command.as_ref())
                    .and_then(Command::changes);
                let keys = |changes: &[Change]| {
                    changes
                        .iter()
                        .map(|c| (c.collection.clone(), c.key.clone()))
                        .collect::<BTreeSet<_>>()
                };
                if !self.index.observes(&id, target)
                    || !structural.contains(target)
                    || !target_changes.is_some_and(|target| keys(target) == keys(changes))
                {
                    refusals.insert(
                        id,
                        "revert does not name a structurally valid observed edit with the same keys".into(),
                    );
                    continue;
                }
            }
            // Structural validity depends only on the authenticated causal past,
            // not on a later request collision by the target's author. Such a
            // collision cannot convert an honest reverter into a faulty writer.
            structural.insert(id);
            if self.conflicts.contains(&id)
                || operation.duplicate_of.is_some()
                || self.opaque_authors.contains(&operation.header.actor)
                || before.is_some_and(|target| target == id || !self.index.observes(&target, &id))
            {
                continue;
            }
            crate::documents::apply(
                &self.index,
                &mut cells,
                id,
                command.changes().unwrap(),
                true,
            );
            eligible.insert(id);
        }
        (cells, refusals, eligible)
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        let (cells, refusals, eligible) = self.project(None);
        let operations = self
            .records
            .iter()
            .map(|(id, (operation, _))| {
                let mut operation = operation.clone();
                operation.eligible = eligible.contains(id);
                if let Some(refusal) = refusals.get(id) {
                    operation.refusal = Some(refusal.clone());
                }
                if self.opaque_authors.contains(&operation.header.actor) {
                    operation.refusal.get_or_insert_with(|| {
                        "author has unavailable retained request history".into()
                    });
                }
                operation
            })
            .collect();
        Snapshot {
            scope: self.scope,
            retained: self.retained,
            withheld: self
                .records
                .values()
                .filter(|(op, _)| !op.catalog_eligible)
                .count(),
            cells,
            operations,
        }
    }
    pub(crate) fn writable(&self, actor: Actor) -> Result<()> {
        if self.snapshot().operations.iter().any(|op| {
            op.header.actor == actor
                && (!op.available || !op.catalog_eligible || op.refusal.is_some())
        }) {
            return Err(Error::new(
                "local proposal writer has withheld, unavailable or refused history; recovery required",
            ));
        }
        Ok(())
    }
    pub(crate) fn retry(&self, actor: Actor, command: &Command) -> Result<Option<Digest>> {
        let mut first = None;
        for (id, (operation, old)) in &self.records {
            if operation.header.actor == actor && operation.request == Some(command.request()) {
                if old.as_ref() != Some(command) || self.conflicts.contains(id) {
                    return Err(Error::new(
                        "request identity reused with different commands",
                    ));
                }
                let candidate = (operation.header.sequence, *id);
                if first.is_none_or(|old| candidate < old) {
                    first = Some(candidate);
                }
            }
        }
        if let Some((_, id)) = first {
            if !self.project(None).2.contains(&id) {
                return Err(Error::new(
                    "the prior proposal receipt is not currently eligible",
                ));
            }
            return Ok(Some(id));
        }
        Ok(None)
    }
    pub(crate) fn revert_retry(
        &self,
        actor: Actor,
        request: [u8; 16],
        target: Digest,
    ) -> Result<Option<Digest>> {
        for (operation, old) in self.records.values() {
            if operation.header.actor == actor && operation.request == Some(request) {
                if let Some(old @ Command::Revert { operation, .. }) = old
                    && *operation == target
                {
                    return self.retry(actor, old);
                }
                return Err(Error::new(
                    "request identity already belongs to another command",
                ));
            }
        }
        Ok(None)
    }
    pub(crate) fn compensate(&self, request: [u8; 16], target: Digest) -> Result<Command> {
        let command = self
            .records
            .get(&target)
            .and_then(|(_, c)| c.as_ref())
            .ok_or_else(|| Error::new("target is not a supported retained proposal"))?;
        let changes = command.changes().unwrap();
        let current = self.snapshot();
        if !current
            .operations
            .iter()
            .any(|op| op.header.id == target && op.eligible)
        {
            return Err(Error::new("target proposal is not eligible"));
        }
        for change in changes {
            if !current
                .cells
                .get(&(change.collection.clone(), change.key.clone()))
                .is_some_and(|cell| {
                    !cell.stopped
                        && cell.candidates.len() == 1
                        && cell.candidates[0].operation == target
                })
            {
                return Err(Error::new(
                    "an affected proposal changed; resolve it before reverting",
                ));
            }
        }
        let (past, _, _) = self.project(Some(target));
        let mut restored = Vec::new();
        for change in changes {
            let old = past.get(&(change.collection.clone(), change.key.clone()));
            if old.is_some_and(|cell| cell.stopped || cell.candidates.len() > 1) {
                return Err(Error::new(
                    "previous proposal is stopped or conflicted; resolve it explicitly",
                ));
            }
            restored.push(Change {
                collection: change.collection.clone(),
                key: change.key.clone(),
                value: old
                    .and_then(|cell| cell.candidates.first())
                    .map(|c| c.value.clone())
                    .filter(|v| !v.is_null()),
            });
        }
        Ok(Command::Revert {
            request,
            operation: target,
            changes: restored,
        })
    }
}
