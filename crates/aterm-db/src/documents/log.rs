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

/// Eligible causal-only merges as (writer, sequence) ([`Log::eligible_and_merges`]).
pub(crate) type Merges = Vec<(Actor, u64)>;

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

impl Snapshot {
    /// All locally retained operations have usable membership, ancestry and
    /// payloads. The store's withheld count includes missing causal parents.
    /// This makes no claim about operations this replica has not seen.
    pub fn history_complete(&self) -> bool {
        self.withheld == 0
            && self
                .operations
                .iter()
                .all(|op| op.available && op.refusal.is_none())
    }
}

pub(crate) struct Log {
    scope: Scope,
    retained: usize,
    append_bound: usize,
    local_use: crate::catalog::ring::Used,
    index: Index,
    records: BTreeMap<Digest, (Operation, Option<Command>)>,
    conflicts: BTreeSet<Digest>,
    opaque_authors: BTreeSet<Actor>,
}

fn decode_with_merge(payload: &[u8], application: fn(&[u8]) -> Result<Command>) -> Result<Command> {
    application(payload)
        .or_else(|error| crate::documents::application::decode_merge(payload).ok_or(error))
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
        let (metadata, append_bound) = store.metadata_with_append_bound(session)?;
        let mut log = Self {
            scope,
            retained: 0,
            append_bound,
            local_use: store.retained_use(session, session.actor())?,
            index: Index::new(crate::database::MAX_RECORDS * 2),
            records: BTreeMap::new(),
            conflicts: BTreeSet::new(),
            opaque_authors: BTreeSet::new(),
        };
        for evidence in metadata {
            let header = evidence.header;
            let id = header.id;
            log.index
                .insert(header.clone())
                .map_err(|_| Error::new("invalid proposal causal history"))?;
            let available = evidence.record.is_some();
            let (command, refusal) = match &evidence.record {
                Some(record) => match decode_with_merge(record.payload(), decode) {
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
        skip: &BTreeSet<Digest>,
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
                if !self.index.observes(&id, target)
                    || !structural.contains(target)
                    || !target_changes.is_some_and(|target| {
                        crate::database::change_keys(target)
                            == crate::database::change_keys(changes)
                    })
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
                || skip.contains(&id)
            {
                continue;
            }
            if !matches!(command, Command::Merge { .. }) {
                crate::documents::apply(
                    &self.index,
                    &mut cells,
                    id,
                    command.changes().unwrap(),
                    true,
                );
            }
            eligible.insert(id);
        }
        (cells, refusals, eligible)
    }
    /// The cells without the operations in `skip`, as if they had never been applied: an
    /// ATT/1 reader passes the automatic records past their writer's quota
    /// (`typed::owned::quota`), which stay in history and dependencies but never project.
    pub(crate) fn cells_without(&self, skip: &BTreeSet<Digest>) -> Cells {
        self.project(None, skip).0
    }
    /// Every eligible business record with its decoded command, in record-id order.
    /// Causal-only merges remain in snapshots and dependencies, not this list. An application
    /// whose records are append-only per writer reads these, never the cells, whose causal
    /// replacement would let one writer's record hide another's.
    pub(crate) fn eligible(&self) -> Vec<(Operation, Command)> {
        self.eligible_and_merges().0
    }
    /// [`Self::eligible`], and each eligible causal-only merge as its writer and sequence: the
    /// joins `application::append` writes before a record on a wide frontier, which a typed
    /// reader charges to that record's writer (`typed::owned::quota`).
    pub(crate) fn eligible_and_merges(&self) -> (Vec<(Operation, Command)>, Merges) {
        let (_, _, eligible) = self.project(None, &BTreeSet::new());
        let mut records = Vec::new();
        let mut merges = Vec::new();
        for (operation, command) in self
            .records
            .iter()
            .filter(|(id, _)| eligible.contains(*id))
            .map(|(_, record)| record)
        {
            match command {
                Some(Command::Merge { .. }) => {
                    merges.push((operation.header.actor, operation.header.sequence));
                }
                Some(command) => records.push((operation.clone(), command.clone())),
                None => {}
            }
        }
        (records, merges)
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        let (cells, refusals, eligible) = self.project(None, &BTreeSet::new());
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
    /// The retained records that could still become visible here, which a reader comparing
    /// against the cells (an ATT/1 grid save, import or seed pass) waits for: a record whose
    /// causal past has not all arrived, with its descendants, and a record of `own` (this
    /// Mac's writer) that is withheld or unavailable. A record this build refuses, a cut-off
    /// or forked writer's and a quarantined payload never project here, and a new append names
    /// only the catalog projection's heads, so such records are inert rather than awaited.
    pub(crate) fn awaited(&self, own: Actor) -> BTreeSet<Digest> {
        let mut children = BTreeMap::<Digest, Vec<Digest>>::new();
        let mut awaited = BTreeSet::new();
        for (id, (operation, _)) in &self.records {
            let header = &operation.header;
            for parent in header.parents.iter().chain(header.previous.iter()) {
                children.entry(*parent).or_default().push(*id);
                if !self.records.contains_key(parent) {
                    awaited.insert(*id);
                }
            }
            if header.actor == own && (!operation.available || !operation.catalog_eligible) {
                awaited.insert(*id);
            }
        }
        let mut todo: Vec<Digest> = awaited.iter().copied().collect();
        while let Some(id) = todo.pop() {
            for child in children.get(&id).into_iter().flatten() {
                if awaited.insert(*child) {
                    todo.push(*child);
                }
            }
        }
        awaited
    }
    pub(crate) fn writable(&self, actor: Actor) -> Result<()> {
        if self.opaque_authors.contains(&actor)
            || self.records.values().any(|(op, _)| {
                op.header.actor == actor
                    && (!op.available || !op.catalog_eligible || op.refusal.is_some())
            })
        {
            return Err(Error::new(
                "local proposal writer has withheld, unavailable or refused history; recovery required",
            ));
        }
        let (_, refusals, _) = self.project(None, &BTreeSet::new());
        if refusals.keys().any(|id| {
            self.records
                .get(id)
                .is_some_and(|(op, _)| op.header.actor == actor)
        }) {
            return Err(Error::new(
                "local proposal writer has withheld, unavailable or refused history; recovery required",
            ));
        }
        Ok(())
    }
    pub(crate) fn retained_counts(&self, actor: Actor) -> (usize, usize) {
        let by_actor = self
            .records
            .values()
            .filter(|(op, _)| op.header.actor == actor)
            .count();
        (self.retained, by_actor)
    }

    /// Retained storage use, including unavailable records, for this reader's writer.
    pub(crate) fn local_use(&self) -> crate::catalog::ring::Used {
        self.local_use
    }

    /// Maximum records the observed local writer needs for one append,
    /// including causal joins. Re-read after any write before admitting another.
    pub(crate) fn append_records_bound(&self) -> usize {
        self.append_bound
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
            if !self.project(None, &BTreeSet::new()).2.contains(&id) {
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
    /// Compensate a target in the application's visible history. Excluded operations retain
    /// their causal links but cannot supply the current value, the target or its preimage.
    pub(crate) fn compensate(
        &self,
        request: [u8; 16],
        target: Digest,
        excluded: &BTreeSet<Digest>,
    ) -> Result<Command> {
        let command = self
            .records
            .get(&target)
            .and_then(|(_, c)| c.as_ref())
            .ok_or_else(|| Error::new("target is not a supported retained proposal"))?;
        let changes = command
            .changes()
            .filter(|changes| !changes.is_empty())
            .ok_or_else(|| Error::new("target proposal has no value changes to revert"))?;
        let (current, _, eligible) = self.project(None, excluded);
        if !eligible.contains(&target) {
            return Err(Error::new("target proposal is not eligible"));
        }
        for change in changes {
            if !current
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
        let (past, _, _) = self.project(Some(target), excluded);
        let mut restored = Vec::with_capacity(changes.len());
        for change in changes {
            let key = (change.collection.clone(), change.key.clone());
            let old = past.get(&key);
            if old.is_some_and(|cell| cell.stopped || cell.candidates.len() > 1) {
                return Err(Error::new(
                    "previous proposal is stopped or conflicted; resolve it explicitly",
                ));
            }
            restored.push(Change {
                collection: key.0,
                key: key.1,
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
