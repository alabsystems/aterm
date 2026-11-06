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
    ordered: Vec<Digest>,
    eligible: BTreeSet<Digest>,
    revert_refusals: BTreeSet<Digest>,
}

const REVERT_REFUSAL: &str =
    "revert does not name a structurally valid observed edit with the same keys";

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
            ordered: Vec::new(),
            eligible: BTreeSet::new(),
            revert_refusals: BTreeSet::new(),
        };
        for evidence in metadata {
            let header = evidence.header;
            let id = header.id;
            log.index
                .insert(header.clone())
                .map_err(|_| Error::new("invalid proposal causal history"))?;
            let available = evidence.record.is_some();
            let (command, refusal) = match &evidence.record {
                // A removal cutoff is table membership, not a document command: a causal
                // parent here that projects nothing and refuses nothing.
                Some(_) if evidence.cutoff => (None, None),
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
            if ids.len() <= 1 {
                continue;
            }
            ids.sort_unstable_by_key(|id| (log.records[id].0.header.sequence, *id));
            let first = ids[0];
            let first_cmd = &log.records[&first].1;
            if ids[1..].iter().any(|id| &log.records[id].1 != first_cmd) {
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
        let withheld = log
            .records
            .iter()
            .filter_map(|(id, (op, _))| (!op.catalog_eligible).then_some(*id))
            .collect();
        let mut structural = BTreeSet::new();
        // Request conflicts are inert application data, not a signed-chain fork.
        // Keep their headers as dependencies so one author cannot suppress the
        // well-formed later edits of every peer that observed the bad request.
        for id in log.index.project_excluding(&withheld).ordered {
            let (operation, Some(command)) = &log.records[&id] else {
                continue;
            };
            if let Command::Revert {
                operation: target,
                changes,
                ..
            } = command
                && (!log.index.observes(&id, target)
                    || !structural.contains(target)
                    || !log
                        .records
                        .get(target)
                        .and_then(|(_, command)| command.as_ref())
                        .and_then(Command::changes)
                        .is_some_and(|target| crate::database::same_change_keys(target, changes)))
            {
                log.revert_refusals.insert(id);
                continue;
            }
            // Structural validity depends only on the authenticated causal past,
            // not on a later request collision by the target's author. Such a
            // collision cannot convert an honest reverter into a faulty writer.
            structural.insert(id);
            if log.conflicts.contains(&id)
                || operation.duplicate_of.is_some()
                || log.opaque_authors.contains(&operation.header.actor)
            {
                continue;
            }
            log.ordered.push(id);
            log.eligible.insert(id);
        }
        Ok(log)
    }
    fn project_cells(&self, before: Option<Digest>, skip: &BTreeSet<Digest>) -> Cells {
        let mut cells = Cells::new();
        for &id in &self.ordered {
            if before.is_some_and(|target| target == id || !self.index.observes(&target, &id))
                || skip.contains(&id)
            {
                continue;
            }
            if let Some(command) = &self.records[&id].1
                && !matches!(command, Command::Merge { .. })
            {
                crate::documents::apply(
                    &self.index,
                    &mut cells,
                    id,
                    command.changes().unwrap(),
                    true,
                );
            }
        }
        cells
    }
    pub(crate) fn history_complete(&self) -> bool {
        self.revert_refusals.is_empty()
            && self.opaque_authors.is_empty()
            && self
                .records
                .values()
                .all(|(op, _)| op.available && op.catalog_eligible && op.refusal.is_none())
    }
    pub(crate) fn project_cell(&self, collection: &str, key: &str) -> Option<Cell> {
        let mut cells = Cells::new();
        for &id in &self.ordered {
            if let Some(command) = &self.records[&id].1
                && !matches!(command, Command::Merge { .. })
            {
                for change in command.changes().unwrap() {
                    if change.collection == collection && change.key == key {
                        crate::documents::apply(
                            &self.index,
                            &mut cells,
                            id,
                            std::slice::from_ref(change),
                            true,
                        );
                    }
                }
            }
        }
        cells.into_values().next()
    }
    /// The cells without the operations in `skip`, as if they had never been applied: an
    /// ATT/1 reader passes the automatic records past their writer's quota
    /// (`typed::owned::quota`), which stay in history and dependencies but never project.
    #[cfg(test)]
    pub(crate) fn cells_without(&self, skip: &BTreeSet<Digest>) -> Cells {
        self.project_cells(None, skip)
    }
    /// The cells without the operations in `skip`, as if they had never been applied (an ATT/1
    /// reader passes the automatic records past their writer's quota, `typed::owned::quota`,
    /// which stay in history and dependencies but never project), together with the retained
    /// operation ids that are unavailable, withheld or refused and not in `awaited`
    /// (`att::View::refused`), computed in one pass without building a full [`Snapshot`].
    pub(crate) fn cells_and_refused(
        &self,
        skip: &BTreeSet<Digest>,
        awaited: &BTreeSet<Digest>,
    ) -> (Cells, Vec<Digest>) {
        let cells = self.project_cells(None, skip);
        let refused = self
            .records
            .iter()
            .filter(|&(id, (op, _))| {
                (!op.available
                    || !op.catalog_eligible
                    || op.refusal.is_some()
                    || self.revert_refusals.contains(id)
                    || self.opaque_authors.contains(&op.header.actor))
                    && !awaited.contains(id)
            })
            .map(|(id, _)| *id)
            .collect();
        (cells, refused)
    }
    pub(crate) const fn retained(&self) -> usize {
        self.retained
    }
    /// Every eligible business record with its decoded command, in record-id order.
    /// Causal-only merges remain in snapshots and dependencies, not this list. An application
    /// whose records are append-only per writer reads these, never the cells, whose causal
    /// replacement would let one writer's record hide another's.
    pub(crate) fn eligible_iter(&self) -> impl Iterator<Item = (&Operation, &Command)> {
        self.eligible.iter().filter_map(|id| {
            let (operation, Some(command)) = self.records.get(id)? else {
                return None;
            };
            (!matches!(command, Command::Merge { .. })).then_some((operation, command))
        })
    }
    pub(crate) fn eligible(&self) -> Vec<(&Operation, &Command)> {
        self.eligible_iter().collect()
    }
    /// Count of eligible decoded operations, including structural merges.
    pub(crate) fn eligible_count(&self) -> usize {
        self.eligible
            .iter()
            .filter(|id| {
                self.records
                    .get(*id)
                    .is_some_and(|(_, command)| command.is_some())
            })
            .count()
    }
    /// [`Self::eligible`] consuming `self` so callers can move decoded commands without cloning.
    pub(crate) fn into_eligible(mut self) -> impl Iterator<Item = (Operation, Command)> {
        self.eligible.into_iter().filter_map(move |id| {
            let (operation, command) = self.records.remove(&id)?;
            match command {
                Some(Command::Merge { .. }) | None => None,
                Some(command) => Some((operation, command)),
            }
        })
    }
    /// [`Self::eligible`], and each eligible causal-only merge as its writer and sequence: the
    /// joins `application::append` writes before a record on a wide frontier, which a typed
    /// reader charges to that record's writer (`typed::owned::quota`).
    pub(crate) fn eligible_and_merges(&self) -> (Vec<(&Operation, &Command)>, Merges) {
        let mut records = Vec::new();
        let mut merges = Vec::new();
        for id in &self.eligible {
            let Some((operation, command)) = self.records.get(id) else {
                continue;
            };
            match command {
                Some(Command::Merge { .. }) => {
                    merges.push((operation.header.actor, operation.header.sequence));
                }
                Some(command) => records.push((operation, command)),
                None => {}
            }
        }
        (records, merges)
    }
    /// Count eligible causal-only merges by `actor` with sequence greater than `last_business`.
    pub(crate) fn pending_merges_after(&self, actor: Actor, last_business: u64) -> usize {
        self.eligible
            .iter()
            .filter(|id| {
                self.records.get(*id).is_some_and(|(operation, command)| {
                    matches!(command, Some(Command::Merge { .. }))
                        && operation.header.actor == actor
                        && operation.header.sequence > last_business
                })
            })
            .count()
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        let cells = self.project_cells(None, &BTreeSet::new());
        let operations = self
            .records
            .iter()
            .map(|(id, (operation, _))| {
                let mut operation = operation.clone();
                operation.eligible = self.eligible.contains(id);
                if self.revert_refusals.contains(id) {
                    operation.refusal = Some(REVERT_REFUSAL.into());
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
        let mut awaited = BTreeSet::new();
        for (id, (operation, _)) in &self.records {
            let header = &operation.header;
            if (header.actor == own && (!operation.available || !operation.catalog_eligible))
                || header
                    .parents
                    .iter()
                    .chain(header.previous.iter())
                    .any(|parent| !self.records.contains_key(parent))
            {
                awaited.insert(*id);
            }
        }
        if awaited.is_empty() {
            return awaited;
        }
        let mut children = BTreeMap::<Digest, Vec<Digest>>::new();
        for (id, (operation, _)) in &self.records {
            let header = &operation.header;
            for parent in header.parents.iter().chain(header.previous.iter()) {
                children.entry(*parent).or_default().push(*id);
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
            || self.revert_refusals.iter().any(|id| {
                self.records
                    .get(id)
                    .is_some_and(|(op, _)| op.header.actor == actor)
            })
        {
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
            if !self.eligible.contains(&id) {
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
        if !self.eligible.contains(&target) || excluded.contains(&target) {
            return Err(Error::new("target proposal is not eligible"));
        }
        let current = self.project_cells(None, excluded);
        let mut keys = Vec::with_capacity(changes.len());
        for change in changes {
            let key = (change.collection.clone(), change.key.clone());
            if !current.get(&key).is_some_and(|cell| {
                !cell.stopped
                    && cell.candidates.len() == 1
                    && cell.candidates[0].operation == target
            }) {
                return Err(Error::new(
                    "an affected proposal changed; resolve it before reverting",
                ));
            }
            keys.push(key);
        }
        let mut past = self.project_cells(Some(target), excluded);
        let mut restored = Vec::with_capacity(keys.len());
        for key in keys {
            let old = past.remove(&key);
            if old
                .as_ref()
                .is_some_and(|cell| cell.stopped || cell.candidates.len() > 1)
            {
                return Err(Error::new(
                    "previous proposal is stopped or conflicted; resolve it explicitly",
                ));
            }
            restored.push(Change {
                collection: key.0,
                key: key.1,
                value: old
                    .and_then(|mut cell| cell.candidates.pop().map(|c| c.value))
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
