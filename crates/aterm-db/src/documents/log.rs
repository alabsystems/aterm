// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::documents::materialized::{Marked, Materialized, Read, materialize};
use crate::{
    Error, Result,
    checkpoint::Tip,
    database::{Cell, Change, Command},
    documents::{Cells, Unwritten},
    table::{
        Scope, Session,
        store::{Receipt, Store},
    },
};
use astream_wire::sync::{Actor, Digest, Header, Index, Projection};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

/// Eligible causal-only merges as (writer, sequence) ([`Log::eligible_and_merges`]).
pub(crate) type Merges = Vec<(Actor, u64)>;

/// Whether `unwritten` names any change of `id`.
fn touches(unwritten: &Unwritten, id: &Digest) -> bool {
    unwritten
        .range((*id, String::new(), String::new())..)
        .next()
        .is_some_and(|(operation, _, _)| operation == id)
}

/// A history clock derived from signed claims and causal edges, not an assertion of UTC time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hlc {
    /// Maximum signed clock claim observed along this operation's causal ancestry.
    pub ms: u64,
    /// Strict causal progress while the maximum claim stays unchanged.
    pub counter: u64,
}

impl Hlc {
    fn after(authored_ms: u64, parent: Option<Self>) -> Result<Self> {
        match parent {
            Some(parent) if authored_ms <= parent.ms => Ok(Self {
                ms: parent.ms,
                counter: parent
                    .counter
                    .checked_add(1)
                    .ok_or_else(|| Error::new("table history logical clock exhausted"))?,
            }),
            _ => Ok(Self {
                ms: authored_ms,
                counter: 0,
            }),
        }
    }
}

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
    /// This makes no claim about operations this replica has not seen. A revert refused for
    /// naming an edit a final checkpoint settled is a judged record, not missing history.
    pub fn history_complete(&self) -> bool {
        self.withheld == 0
            && self.operations.iter().all(|op| {
                op.available
                    && op
                        .refusal
                        .as_deref()
                        .is_none_or(|refusal| refusal == SETTLED_REVERT_REFUSAL)
            })
    }
}

pub(crate) struct Log {
    scope: Scope,
    retained: usize,
    append_bound: usize,
    local_use: crate::catalog::ring::Used,
    index: Index,
    /// The projection of `index` without the catalog-withheld records, whose version vectors
    /// answer [`Self::observes`] without walking the graph.
    projection: Projection,
    records: BTreeMap<Digest, (Operation, Option<Command>)>,
    /// The frontier of the floor the judgments below are made above: for a read, the settled
    /// floor S's, the newest final checkpoint's ([`crate::checkpoint::finals`]); for a state's
    /// digest, the cut it is of ([`Self::settled_at`]). A record at or below it is settled.
    floor: Option<BTreeMap<Actor, Tip>>,
    conflicts: BTreeSet<Digest>,
    /// The writers with an unavailable record above the floor.
    opaque_authors: BTreeSet<Actor>,
    /// The records of `opaque_authors` above the floor, which their writers' unavailable
    /// records hold back.
    opaque: BTreeSet<Digest>,
    ordered: Vec<Digest>,
    eligible: BTreeSet<Digest>,
    /// Each refused revert, with what refuses it.
    revert_refusals: BTreeMap<Digest, &'static str>,
    /// Where the cells this log last took from [`MATERIALIZED`] stand
    /// ([`Self::cells_marked`]).
    marked: Mutex<Option<Marked>>,
}

/// The last cells each recently read table made in this process, so the next read of a table
/// applies only what arrived since ([`crate::documents::materialized`]). An entry is taken out
/// while a read extends it, so two reads of one table never share one. Past
/// [`MATERIALIZED_TABLES`] the table with the fewest cells goes first: it costs the least to
/// read in full again, and a large table read often stays however many small ones pass.
static MATERIALIZED: Mutex<Vec<(Scope, Materialized)>> = Mutex::new(Vec::new());

/// Drop the cells [`MATERIALIZED`] keeps, as a process that read no table.
#[cfg(test)]
pub(crate) fn forget_for_test() {
    if let Ok(mut held) = MATERIALIZED.lock() {
        held.clear();
    }
}

/// Tables whose cells [`MATERIALIZED`] keeps. A memory bound on a cache, not on any table: a
/// table that falls out of it is read in full once, then kept again.
const MATERIALIZED_TABLES: usize = 32;

const REVERT_REFUSAL: &str =
    "revert does not name a structurally valid observed edit with the same keys";

/// A revert above a final checkpoint naming an edit at or below it (decision LT-B10D-REVERT):
/// the checkpoint's cells pin what the edit wrote, so its writer writes the value it wants.
pub(crate) const SETTLED_REVERT_REFUSAL: &str =
    "revert names an edit a final checkpoint settled; write the value it would restore instead";

/// What the snapshot says of a record its writer's unavailable record holds back.
const OPAQUE_REFUSAL: &str = "author has unavailable retained request history";

const REQUEST_REFUSAL: &str = "request identity reused with different commands";

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
            // The held records, each counted once: no count of the log's own.
            index: Index::new(metadata.len()),
            projection: Projection::default(),
            records: BTreeMap::new(),
            floor: None,
            conflicts: BTreeSet::new(),
            opaque_authors: BTreeSet::new(),
            opaque: BTreeSet::new(),
            ordered: Vec::new(),
            eligible: BTreeSet::new(),
            revert_refusals: BTreeMap::new(),
            marked: Mutex::new(None),
        };
        for evidence in metadata {
            let control = evidence.is_table_control();
            let header = evidence.header;
            let id = header.id;
            log.index
                .insert(header.clone())
                .map_err(|_| Error::new("invalid proposal causal history"))?;
            let available = evidence.record.is_some();
            let (command, refusal) = match &evidence.record {
                // A removal cutoff is table membership and a checkpoint is table compaction,
                // neither a document command: a causal parent here that projects nothing and
                // refuses nothing.
                Some(_) if control => (None, None),
                Some(record) => match decode_with_merge(record.payload(), decode) {
                    Ok(command) => (Some(command), None),
                    Err(error) => (None, Some(error.0)),
                },
                None => (None, None),
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
        log.retained = log.records.len();
        let withheld = log
            .records
            .iter()
            .filter_map(|(id, (op, _))| (!op.catalog_eligible).then_some(*id))
            .collect();
        log.projection = log.index.project_excluding(&withheld);
        // The settled floor and the final checkpoints below it, from what every replica holding
        // their tips holds alike: the checkpoint records, the membership and the tips.
        let finals = crate::checkpoint::finals(
            store.checkpoints(),
            scope.digest(),
            scope.generation(),
            &session.member_actors(),
            |actor, tip| log.tip_ms(actor, tip),
        );
        let floor = finals
            .into_iter()
            .next_back()
            .map(|(body, _)| body.frontier);
        log.judge(floor);
        Ok(log)
    }

    /// The authored time of the record `tip` names, where this log holds it as `actor`'s record
    /// at the tip's sequence: what a checkpoint's finality reads of its cut.
    pub(crate) fn tip_ms(&self, actor: &Actor, tip: &Tip) -> Option<u64> {
        self.records
            .get(&tip.digest)
            .filter(|(operation, _)| {
                operation.header.actor == *actor && operation.header.sequence == tip.sequence
            })
            .map(|(operation, _)| operation.authored_ms)
    }

    /// The frontier of the floor this log was judged above: for a read, the settled floor S's
    /// ([`crate::checkpoint::finals`]).
    pub(crate) const fn settled_frontier(&self) -> Option<&BTreeMap<Actor, Tip>> {
        self.floor.as_ref()
    }

    /// Whether the cut at `frontier` holds the retained record `id`: its writer's sequence is
    /// within the frontier and a tip observes it or is it.
    fn within(&self, frontier: &BTreeMap<Actor, Tip>, id: &Digest) -> bool {
        let Some((operation, _)) = self.records.get(id) else {
            return false;
        };
        let header = &operation.header;
        let Some(own) = frontier.get(&header.actor) else {
            return false;
        };
        header.sequence <= own.sequence
            && (own.digest == *id
                || self.observes(&own.digest, id)
                || frontier
                    .values()
                    .any(|tip| tip.digest != own.digest && self.observes(&tip.digest, id)))
    }

    /// Whether the floor holds the retained record `id`: nothing above it changes how `id` is
    /// judged.
    pub(crate) fn is_settled(&self, id: &Digest) -> bool {
        self.floor
            .as_ref()
            .is_some_and(|frontier| self.within(frontier, id))
    }

    /// Judge every retained record on either side of `floor` (design §5, "Nothing above S
    /// changes what S's state holds", tracker LT-B10D1), so no record above it reaches one at or
    /// below it:
    ///
    /// - request identities are compared only between records on one side, so a record above
    ///   the floor that reuses the request id of one at or below it is not compared with it,
    ///   and neither is marked;
    /// - a revert above the floor of an edit at or below it is refused
    ///   ([`SETTLED_REVERT_REFUSAL`]);
    /// - an unavailable record above the floor makes its writer's records above it ineligible;
    ///   an unavailable record at or below it makes none so.
    ///
    /// None of these judgments of a record at or below the floor then reads a record above it,
    /// so they are the judgments the state the floor's checkpoint signed made
    /// ([`Self::settled_at`]); what the store withholds from the catalog projection (membership,
    /// cutoffs, quarantine) is judged before this, from the control history. Records at or below
    /// the floor are still awaited ([`Self::awaited`]) while a read folds them. With no floor
    /// every record is on one side and the judgments are those of a log without checkpoints.
    fn judge(&mut self, floor: Option<BTreeMap<Actor, Tip>>) {
        self.floor = floor;
        self.conflicts.clear();
        self.opaque_authors.clear();
        self.opaque.clear();
        for (operation, command) in self.records.values_mut() {
            operation.duplicate_of = None;
            // A refusal of a record with a command is a request judgment; one without is its
            // payload's, which no judgment here changes.
            if command.is_some() {
                operation.refusal = None;
            }
        }
        self.opaque_authors = self
            .records
            .iter()
            .filter(|(id, (operation, _))| !operation.available && !self.is_settled(id))
            .map(|(_, (operation, _))| operation.header.actor)
            .collect();
        if !self.opaque_authors.is_empty() {
            self.opaque = self
                .records
                .iter()
                .filter(|(id, (operation, _))| {
                    self.opaque_authors.contains(&operation.header.actor) && !self.is_settled(id)
                })
                .map(|(id, _)| *id)
                .collect();
        }
        let mut requests = BTreeMap::<(Actor, [u8; 16]), Vec<Digest>>::new();
        for (id, (operation, command)) in &self.records {
            if let Some(command) = command {
                requests
                    .entry((operation.header.actor, command.request()))
                    .or_default()
                    .push(*id);
            }
        }
        let mut conflicts = Vec::new();
        let mut duplicates = Vec::new();
        for ids in requests.into_values().filter(|ids| ids.len() > 1) {
            let (settled, hot): (Vec<Digest>, Vec<Digest>) =
                ids.into_iter().partition(|id| self.is_settled(id));
            for mut ids in [settled, hot].into_iter().filter(|ids| ids.len() > 1) {
                ids.sort_unstable_by_key(|id| (self.records[id].0.header.sequence, *id));
                let first = ids[0];
                let first_cmd = &self.records[&first].1;
                if ids[1..].iter().any(|id| &self.records[id].1 != first_cmd) {
                    conflicts.extend(ids);
                } else {
                    duplicates.extend(ids[1..].iter().map(|id| (*id, first)));
                }
            }
        }
        for id in conflicts {
            self.conflicts.insert(id);
            if let Some((operation, _)) = self.records.get_mut(&id) {
                operation.refusal = Some(REQUEST_REFUSAL.into());
            }
        }
        for (id, first) in duplicates {
            if let Some((operation, _)) = self.records.get_mut(&id) {
                operation.duplicate_of = Some(first);
            }
        }
        let mut structural = BTreeSet::new();
        let mut revert_refusals = BTreeMap::new();
        let mut ordered = Vec::new();
        let mut eligible = BTreeSet::new();
        // Request conflicts are inert application data, not a signed-chain fork.
        // Keep their headers as dependencies so one author cannot suppress the
        // well-formed later edits of every peer that observed the bad request.
        for id in &self.projection.ordered {
            let (operation, Some(command)) = &self.records[id] else {
                continue;
            };
            if let Command::Revert {
                operation: target,
                changes,
                ..
            } = command
            {
                // A revert of a revert refused for reaching below the floor reaches below it too.
                let refusal = if (self.is_settled(target) && !self.is_settled(id))
                    || revert_refusals.get(target) == Some(&SETTLED_REVERT_REFUSAL)
                {
                    Some(SETTLED_REVERT_REFUSAL)
                } else if !self.observes(id, target)
                    || !structural.contains(target)
                    || !self
                        .records
                        .get(target)
                        .and_then(|(_, command)| command.as_ref())
                        .and_then(Command::changes)
                        .is_some_and(|target| crate::database::same_change_keys(target, changes))
                {
                    Some(REVERT_REFUSAL)
                } else {
                    None
                };
                if let Some(refusal) = refusal {
                    revert_refusals.insert(*id, refusal);
                    continue;
                }
            }
            // Structural validity depends only on the authenticated causal past,
            // not on a later request collision by the target's author. Such a
            // collision cannot convert an honest reverter into a faulty writer.
            structural.insert(*id);
            if self.conflicts.contains(id)
                || operation.duplicate_of.is_some()
                || self.opaque.contains(id)
            {
                continue;
            }
            ordered.push(*id);
            eligible.insert(*id);
        }
        self.revert_refusals = revert_refusals;
        self.ordered = ordered;
        self.eligible = eligible;
    }

    /// This log read again as though the retained record `id` were quarantined here: what
    /// [`Self::read_with`] makes of the evidence a store gives for a quarantined payload
    /// (`Store::metadata_from`): `id` unavailable, and `id`, each later record of its writer
    /// and each record depending on one withheld from the catalog projection, as the store's
    /// quarantine closure withholds them (`Store::quarantine_closure`).
    #[cfg(test)]
    pub(crate) fn with_quarantined(&self, id: &Digest) -> Self {
        let mut log = self.as_of_cut(&self.records.keys().copied().collect());
        let Some((writer, sequence)) = log
            .records
            .get(id)
            .map(|(operation, _)| (operation.header.actor, operation.header.sequence))
        else {
            return log;
        };
        if let Some((operation, command)) = log.records.get_mut(id) {
            operation.available = false;
            operation.request = None;
            operation.reverts = None;
            operation.refusal = None;
            *command = None;
        }
        let mut withheld: BTreeSet<Digest> = log
            .records
            .iter()
            .filter(|(_, (operation, _))| {
                operation.header.actor == writer && operation.header.sequence >= sequence
            })
            .map(|(id, _)| *id)
            .collect();
        loop {
            let more: Vec<Digest> = log
                .records
                .iter()
                .filter(|(id, (operation, _))| {
                    !withheld.contains(*id)
                        && operation
                            .header
                            .parents
                            .iter()
                            .chain(operation.header.previous.iter())
                            .any(|parent| withheld.contains(parent))
                })
                .map(|(id, _)| *id)
                .collect();
            if more.is_empty() {
                break;
            }
            withheld.extend(more);
        }
        for id in &withheld {
            if let Some((operation, _)) = log.records.get_mut(id) {
                operation.catalog_eligible = false;
            }
        }
        log.projection = log.index.project_excluding(&withheld);
        let floor = log.floor.clone();
        log.judge(floor);
        log
    }

    /// This log judged with the cut `cut` (a causally closed set of its records) as its floor:
    /// what a read will judge of the cut's records once a checkpoint of it is final, so a
    /// checkpoint's state ([`crate::att::checkpoint::state`]) is what a fold from it reads. Its
    /// request, revert and opaque-author judgments of the cut's records then read the cut alone,
    /// not the records above it nor which checkpoints this replica finds final.
    pub(crate) fn settled_at(&self, cut: &BTreeSet<Digest>) -> Self {
        let mut frontier = BTreeMap::<Actor, Tip>::new();
        for id in cut {
            let Some((operation, _)) = self.records.get(id) else {
                continue;
            };
            let header = &operation.header;
            let tip = frontier.entry(header.actor).or_insert(Tip {
                sequence: 0,
                digest: [0; 32],
            });
            if header.sequence > tip.sequence {
                *tip = Tip {
                    sequence: header.sequence,
                    digest: header.id,
                };
            }
        }
        let mut log = self.as_of_cut(&self.records.keys().copied().collect());
        log.judge(Some(frontier));
        log
    }
    /// The log as of the causal cut `at` names: `at` and every retained operation it observed
    /// through its signed parents and its writer's chain, and nothing else. An operation's
    /// eligibility, refusal and duplicate judgement stay as this replica judges them now, on
    /// everything it retains; only what the cut leaves out is gone, so a reader of the cut
    /// ([`crate::att::View::read`]) projects the table as it stood there. The authored clock
    /// plays no part: it is a claim, and a cut is the signed graph's.
    ///
    /// # Errors
    /// This replica retains no operation `at` in this table.
    pub(crate) fn as_of(&self, at: &Digest) -> Result<Self> {
        if !self.records.contains_key(at) {
            return Err(Error::new(
                "no operation with that digest in this table here (see `aterm db table history`)",
            ));
        }
        let mut cut = BTreeSet::new();
        let mut todo = vec![*at];
        while let Some(id) = todo.pop() {
            let Some((operation, _)) = self.records.get(&id) else {
                continue;
            };
            if !cut.insert(id) {
                continue;
            }
            todo.extend(
                operation
                    .header
                    .parents
                    .iter()
                    .chain(operation.header.previous.iter())
                    .filter(|parent| !cut.contains(*parent)),
            );
        }
        Ok(self.as_of_cut(&cut))
    }
    /// The log restricted to `cut`, a causally closed set of its operations ([`Self::as_of`]
    /// for one operation's cut; a table checkpoint's frontier for several writers',
    /// [`crate::checkpoint`]), judged as [`Self::as_of`] judges.
    pub(crate) fn as_of_cut(&self, cut: &BTreeSet<Digest>) -> Self {
        Self {
            scope: self.scope,
            retained: self.retained,
            append_bound: self.append_bound,
            local_use: self.local_use,
            index: self.index.clone(),
            projection: self.projection.clone(),
            records: self
                .records
                .iter()
                .filter(|(id, _)| cut.contains(*id))
                .map(|(id, record)| (*id, record.clone()))
                .collect(),
            floor: self.floor.clone(),
            conflicts: self.conflicts.intersection(cut).copied().collect(),
            // A writer is opaque in the cut only by an unavailable record the cut holds: one
            // above it is gone with the rest, so a state of the cut is the cut's alone.
            opaque_authors: self
                .records
                .iter()
                .filter(|(id, (operation, _))| {
                    cut.contains(*id)
                        && !operation.available
                        && !self.is_settled(id)
                        && self.opaque_authors.contains(&operation.header.actor)
                })
                .map(|(_, (operation, _))| operation.header.actor)
                .collect(),
            opaque: self.opaque.intersection(cut).copied().collect(),
            ordered: self
                .ordered
                .iter()
                .filter(|id| cut.contains(*id))
                .copied()
                .collect(),
            eligible: self.eligible.intersection(cut).copied().collect(),
            revert_refusals: self
                .revert_refusals
                .iter()
                .filter(|(id, _)| cut.contains(*id))
                .map(|(id, refusal)| (*id, *refusal))
                .collect(),
            // A cut's cells are its own, never the ones a read of the whole table kept.
            marked: Mutex::new(None),
        }
    }
    /// [`Index::observes`] of this log's index, answered from the projection's version vectors
    /// where `later` has a row and by a bounded walk otherwise (astream's
    /// `Projection::observes`), so a read applying `n` operations costs no walk per candidate.
    pub(crate) fn observes(&self, later: &Digest, earlier: &Digest) -> bool {
        self.projection.observes(&self.index, later, earlier)
    }
    /// The projection [`Self::observes`] answers from: this log's order, quarantines and version
    /// vectors.
    pub(crate) const fn projection(&self) -> &Projection {
        &self.projection
    }
    /// The signed authoring time of the retained operation `id`.
    pub(crate) fn authored_ms(&self, id: &Digest) -> Option<u64> {
        self.records
            .get(id)
            .map(|(operation, _)| operation.authored_ms)
    }
    /// Whether this log (a cut of one, [`Self::as_of`]) holds the retained operation `id`.
    pub(crate) fn holds(&self, id: &Digest) -> bool {
        self.records.contains_key(id)
    }
    /// The retained operation `id`, whatever its eligibility.
    pub(crate) fn operation(&self, id: &Digest) -> Option<&Operation> {
        self.records.get(id).map(|(operation, _)| operation)
    }
    /// What the retained record `id` says of who made it, why and on what evidence, where it
    /// is a version 2 record its generation decoded ([`crate::said`]).
    pub(crate) fn said(&self, id: &Digest) -> Option<&crate::said::Said> {
        self.records.get(id)?.1.as_ref()?.said()
    }
    /// Who signed the retained record `id` (the roster it names and its writer) and why this
    /// build does not project it, where [`Self::refused`] counts it refused: its
    /// payload's refusal in this build's words, a revert this replica refuses, a payload not
    /// readable here, a writer cut off or forked, or a record above the settled floor that its
    /// writer's unreadable record there holds back.
    pub(crate) fn refusal(&self, id: &Digest) -> Option<(Digest, Actor, String)> {
        let (op, _) = self.records.get(id)?;
        let why = if let Some(refusal) = &op.refusal {
            refusal.clone()
        } else if let Some(why) = self.revert_refusals.get(id) {
            (*why).to_owned()
        } else if !op.available {
            "its payload cannot be read here".to_owned()
        } else if !op.catalog_eligible {
            "its writer was cut off or forked".to_owned()
        } else if self.opaque.contains(id) {
            "its writer has a record that cannot be read here".to_owned()
        } else {
            return None;
        };
        Some((op.roster, op.header.actor, why))
    }
    /// The roster and writer of each retained record [`Self::refusal`] names.
    pub(crate) fn refused_writers(&self) -> impl Iterator<Item = (Digest, Actor)> + '_ {
        self.records
            .keys()
            .filter_map(|id| self.refusal(id).map(|(roster, actor, _)| (roster, actor)))
    }

    /// Each of `ids` [`Self::refusal`] names, with its signer's machine as `resolve` finds it
    /// through the roster the record names.
    pub(crate) fn refusals(
        &self,
        ids: &[Digest],
        resolve: &dyn Fn(&Digest, &Actor) -> Option<[u8; 16]>,
    ) -> Vec<crate::typed::Unread> {
        ids.iter()
            .filter_map(|id| {
                let (roster, actor, why) = self.refusal(id)?;
                Some(crate::typed::Unread {
                    operation: *id,
                    actor,
                    machine: resolve(&roster, &actor),
                    why,
                })
            })
            .collect()
    }
    fn project_cells(
        &self,
        before: Option<Digest>,
        skip: &BTreeSet<Digest>,
        unwritten: &Unwritten,
    ) -> Cells {
        let mut cells = Cells::new();
        let mut kept = Vec::new();
        for &id in &self.ordered {
            if before.is_some_and(|target| target == id || !self.observes(&target, &id))
                || skip.contains(&id)
            {
                continue;
            }
            if let Some(command) = &self.records[&id].1
                && !matches!(command, Command::Merge { .. })
            {
                let changes = command.changes().unwrap();
                let changes = if touches(unwritten, &id) {
                    kept.clear();
                    kept.extend(
                        changes
                            .iter()
                            .filter(|change| {
                                !unwritten.contains(&(
                                    id,
                                    change.collection.clone(),
                                    change.key.clone(),
                                ))
                            })
                            .cloned(),
                    );
                    kept.as_slice()
                } else {
                    changes
                };
                crate::documents::apply(
                    &|later, earlier| self.observes(later, earlier),
                    &mut cells,
                    id,
                    changes,
                    true,
                );
            }
        }
        cells
    }
    /// Every record this log retains, by operation digest, eligible or not. A log only grows, so
    /// a later read's set contains an earlier one's.
    #[cfg(test)]
    pub(crate) fn retained_ids(&self) -> impl Iterator<Item = &Digest> {
        self.records.keys()
    }
    /// Whether this log retains the record `id`, eligible or not.
    pub(crate) fn retains(&self, id: &Digest) -> bool {
        self.records.contains_key(id)
    }
    /// The refused reverts a writer is answerable for: every one but a revert of an edit a final
    /// checkpoint settled, which a well-formed revert written before its writer found the
    /// checkpoint final can become ([`SETTLED_REVERT_REFUSAL`]), and which no recovery undoes.
    fn faulty_reverts(&self) -> impl Iterator<Item = &Digest> {
        self.revert_refusals
            .iter()
            .filter(|(_, refusal)| **refusal != SETTLED_REVERT_REFUSAL)
            .map(|(id, _)| id)
    }
    /// A digest of how this replica judges each retained record now: the judgments that can
    /// move while the records do not (a payload key this Mac comes to hold, a membership change
    /// that makes a cutoff count), in record-id order, and the floor the request, revert and
    /// opaque-author judgments are made above ([`Self::judge`]), which a membership change can
    /// move too. With the retained ids (which commit to
    /// each authenticated record, so to everything decoded from it) it fixes what a projection
    /// of the log reads: the shared files' projection key (`file::materialize`).
    pub(crate) fn judgment(&self) -> [u8; 32] {
        let mut sha = aterm_digest::Sha256::new();
        sha.update(b"aterm/db/log/judgment/2\0");
        sha.update((self.records.len() as u64).to_le_bytes());
        for (id, (operation, _command)) in &self.records {
            // Every field takes a position: a new one does not compile until it does.
            let Operation {
                // Fixed by the id, or derived from the record's bytes and the judgments below.
                header: _,
                roster: _,
                epoch: _,
                authored_ms: _,
                request: _,
                reverts: _,
                duplicate_of: _,
                // This replica's first retention: no projection reads it.
                receipt: _,
                available,
                catalog_eligible,
                eligible,
                refusal,
            } = operation;
            sha.update(id);
            sha.update([
                u8::from(*available),
                u8::from(*catalog_eligible),
                u8::from(*eligible),
            ]);
            match refusal {
                None => sha.update([0]),
                Some(why) => {
                    sha.update([1]);
                    sha.update((why.len() as u64).to_le_bytes());
                    sha.update(why.as_bytes());
                }
            }
        }
        match &self.floor {
            None => sha.update([0]),
            Some(frontier) => {
                sha.update([1]);
                sha.update((frontier.len() as u64).to_le_bytes());
                for (actor, tip) in frontier {
                    sha.update(actor);
                    sha.update(tip.sequence.to_le_bytes());
                    sha.update(tip.digest);
                }
            }
        }
        sha.finalize()
    }
    pub(crate) fn history_complete(&self) -> bool {
        self.faulty_reverts().next().is_none()
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
                            &|later, earlier| self.observes(later, earlier),
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
    /// The cells without the operations in `skip` and the single changes in `unwritten`, as if
    /// they had never been applied: an ATT/1 reader passes the automatic records past their
    /// writer's quota (`typed::owned::quota`) and the writes the authority rules hold inert
    /// (`typed::authority`), which stay in history and dependencies but never project. Where
    /// `before` names an operation, only what it observed, as it observed it. With no `before`
    /// and nothing unwritten, the cells extend from this table's last read
    /// ([`Self::materialized_cells`]); otherwise they are replayed in full.
    pub(crate) fn cells_without(
        &self,
        before: Option<Digest>,
        skip: &BTreeSet<Digest>,
        unwritten: &Unwritten,
    ) -> Cells {
        if before.is_none() && unwritten.is_empty() {
            self.materialized_cells(skip)
        } else {
            self.project_cells(before, skip, unwritten)
        }
    }
    /// The retained records this build never projects and that cannot become visible, none of
    /// `awaited` among them (`att::View::refused`): each [`Self::refusal`] names, without
    /// building a full [`Snapshot`].
    pub(crate) fn refused(&self, awaited: &BTreeSet<Digest>) -> Vec<Digest> {
        self.records
            .keys()
            .filter(|id| !awaited.contains(*id) && self.refusal(id).is_some())
            .copied()
            .collect()
    }
    /// The cells without `skip`, extended from this table's last read in this process where
    /// [`materialize`] may reuse it, and kept for the next.
    fn materialized_cells(&self, skip: &BTreeSet<Digest>) -> Cells {
        let previous = MATERIALIZED.lock().ok().and_then(|mut held| {
            let at = held.iter().position(|(scope, _)| *scope == self.scope)?;
            Some(held.remove(at).1)
        });
        let made = materialize(
            previous,
            Read {
                retained: self.records.keys().copied().collect(),
                ordered: &self.ordered,
                skip,
                quarantined: &self.projection.quarantined,
            },
            |id| {
                self.records
                    .get(id)
                    .and_then(|(_, command)| command.as_ref())
                    .filter(|command| !matches!(command, Command::Merge { .. }))
                    .and_then(Command::changes)
            },
            &|later, earlier| self.observes(later, earlier),
        );
        let cells = made.cells.clone();
        if let Ok(mut marked) = self.marked.lock() {
            *marked = Some(made.marked());
        }
        if let Ok(mut held) = MATERIALIZED.lock() {
            held.insert(0, (self.scope, made));
            while held.len() > MATERIALIZED_TABLES {
                // The fewest cells, the oldest of those (the last).
                let Some(least) = held
                    .iter()
                    .enumerate()
                    .rev()
                    .min_by_key(|(_, (_, made))| made.cells.len())
                    .map(|(at, _)| at)
                else {
                    break;
                };
                held.remove(least);
            }
        }
        cells
    }
    pub(crate) const fn retained(&self) -> usize {
        self.retained
    }
    /// Where the cells this log last took from the cells this process keeps of its table stand
    /// ([`crate::documents::materialized::Marked`]); `None` where it took none.
    pub(crate) fn cells_marked(&self) -> Option<Marked> {
        self.marked.lock().ok().and_then(|marked| marked.clone())
    }
    /// Authenticated ancestry, including causal-only merges that carry no cell writes.
    pub(crate) const fn causal_index(&self) -> &Index {
        &self.index
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

    /// Every eligible business record as (derived clock, operation digest), in that order: the
    /// sort key of history (`att::history`). The clocks are folded over the whole authenticated
    /// index, structural and refused records included, since their signed parents still
    /// constrain the edits that observed them; causal-only merges are left out of the order.
    /// Nothing is copied out of a record: a reader of one page looks up only the records it keeps
    /// ([`Self::eligible_record`]).
    ///
    /// # Errors
    /// An authenticated causal parent has no derived clock, or its logical counter overflows.
    pub(crate) fn causal_order(&self) -> Result<Vec<(Hlc, Digest)>> {
        let withheld = self
            .records
            .iter()
            .filter_map(|(id, (op, _))| (!op.catalog_eligible).then_some(*id))
            .collect();
        let mut clocks = BTreeMap::new();
        for id in self.index.project_excluding(&withheld).ordered {
            let operation = &self.records[&id].0;
            let mut parent_clock = None;
            for parent in operation
                .header
                .parents
                .iter()
                .chain(operation.header.previous.iter())
            {
                let clock = *clocks
                    .get(parent)
                    .ok_or_else(|| Error::new("table history causal clock is incomplete"))?;
                parent_clock =
                    Some(parent_clock.map_or(clock, |previous: Hlc| previous.max(clock)));
            }
            clocks.insert(id, Hlc::after(operation.authored_ms, parent_clock)?);
        }
        let mut ordered = Vec::with_capacity(self.eligible.len());
        for id in &self.eligible {
            let clock = clocks
                .get(id)
                .copied()
                .ok_or_else(|| Error::new("table history causal clock is incomplete"))?;
            if self.eligible_record(id).is_some() {
                ordered.push((clock, *id));
            }
        }
        ordered.sort_unstable();
        Ok(ordered)
    }

    /// The eligible business record `id` and its decoded command; `None` for a causal-only
    /// merge, an ineligible record or one this log does not hold.
    pub(crate) fn eligible_record(&self, id: &Digest) -> Option<(&Operation, &Command)> {
        if !self.eligible.contains(id) {
            return None;
        }
        match self.records.get(id)? {
            (_, Some(Command::Merge { .. }) | None) => None,
            (operation, Some(command)) => Some((operation, command)),
        }
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
    pub(crate) fn snapshot(&self) -> Snapshot {
        let cells = self.project_cells(None, &BTreeSet::new(), &Unwritten::new());
        let operations = self
            .records
            .iter()
            .map(|(id, (operation, _))| {
                let mut operation = operation.clone();
                operation.eligible = self.eligible.contains(id);
                match self.revert_refusals.get(id) {
                    // A settled refusal never hides another the record has (a reused request
                    // id), which its writer answers for.
                    Some(&SETTLED_REVERT_REFUSAL) => {
                        operation
                            .refusal
                            .get_or_insert_with(|| SETTLED_REVERT_REFUSAL.into());
                    }
                    Some(refusal) => operation.refusal = Some((*refusal).into()),
                    None => {}
                }
                if self.opaque.contains(id) {
                    operation
                        .refusal
                        .get_or_insert_with(|| OPAQUE_REFUSAL.into());
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
    /// only the catalog projection's heads, so such records are inert rather than awaited. A
    /// settled record ([`Self::is_settled`]) is awaited like any other: every record this
    /// awaits is missing from the cells this log folds, a settled one too, until a read takes the
    /// cells at the floor from the checkpoint's state (tracker LT-B10D2).
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
    /// The rows (collection, primary key) the `awaited` records write, from their decoded
    /// commands; `None` where one has no readable command, so its rows are unknown. A merge
    /// writes none; a revert writes the keys of its compensation.
    pub(crate) fn awaited_rows(
        &self,
        awaited: &BTreeSet<Digest>,
    ) -> Option<BTreeSet<(String, String)>> {
        let mut rows = BTreeSet::new();
        for id in awaited {
            let (_, command) = self.records.get(id)?;
            for change in command.as_ref()?.changes()? {
                let pk = change
                    .key
                    .split_once('.')
                    .map_or(change.key.as_str(), |(pk, _)| pk);
                rows.insert((change.collection.clone(), pk.to_owned()));
            }
        }
        Some(rows)
    }
    /// The writers [`Self::writable`] refuses, judged from the retained records `of` keeps: each
    /// with a withheld, unavailable or refused record among them or a refused revert it is
    /// answerable for, and each with an unavailable record above the floor. A typed read's state
    /// keeps them (`typed::state::State::refused`): the records at or below a floor are judged
    /// by the floor's cut alone ([`Self::judge`]), so the writers they refuse at the cut and the
    /// writers the records above refuse are together the writers every record refuses.
    pub(crate) fn unwritable(&self, of: impl Fn(&Digest) -> bool) -> BTreeSet<Actor> {
        let mut writers = self.opaque_authors.clone();
        for (id, (op, _)) in &self.records {
            if of(id) && (!op.available || !op.catalog_eligible || op.refusal.is_some()) {
                writers.insert(op.header.actor);
            }
        }
        for id in self.faulty_reverts().filter(|id| of(id)) {
            if let Some((op, _)) = self.records.get(id) {
                writers.insert(op.header.actor);
            }
        }
        writers
    }
    pub(crate) fn writable(&self, actor: Actor) -> Result<()> {
        if self.unwritable(|_| true).contains(&actor) {
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
        Ok(self
            .resolve_request(actor, command.request(), |old| Ok(old == command))?
            .map(|(id, _)| id))
    }

    /// Resolve an exact request from all retained records, not only the visible
    /// candidates. Identical copies choose the original eligible receipt;
    /// a conflicting or unavailable command cannot masquerade as its retry.
    pub(crate) fn resolve_request(
        &self,
        actor: Actor,
        request: [u8; 16],
        matches: impl Fn(&Command) -> Result<bool>,
    ) -> Result<Option<(Digest, &Command)>> {
        let mut first = None;
        for (id, (operation, old)) in &self.records {
            if operation.header.actor == actor && operation.request == Some(request) {
                let matched = match old {
                    Some(old) => matches(old)?,
                    None => false,
                };
                if !matched || self.conflicts.contains(id) {
                    return Err(Error::new(REQUEST_REFUSAL));
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
            let command = self.records[&id]
                .1
                .as_ref()
                .ok_or_else(|| Error::new("proposal command is unavailable"))?;
            return Ok(Some((id, command)));
        }
        Ok(None)
    }
    /// The earliest retained record published under `request`, by any actor of this table.
    ///
    /// [`Self::retry`] scopes a request to the actor that asks, so it does not find the record
    /// a writer signed before its succession: its successor is another actor, and the imported
    /// record still names the first. A shared file's publisher asks this before it signs an
    /// intent, whose id is its record's request, so an intent a predecessor (or a pass killed
    /// before its marker) already signed is never signed again. Eligibility is not asked: a
    /// retained record that is withheld now may become eligible, and a second one would then
    /// stand beside it.
    pub(crate) fn carried(&self, request: [u8; 16]) -> Option<Digest> {
        self.records
            .iter()
            .filter(|(_, (operation, _))| operation.request == Some(request))
            .map(|(id, (operation, _))| (operation.header.sequence, *id))
            .min()
            .map(|(_, id)| id)
    }
    /// The value changes of the retained record `id`, where it is one this build reads.
    pub(crate) fn record_changes(&self, id: &Digest) -> Option<&[Change]> {
        self.records
            .get(id)
            .and_then(|(_, command)| command.as_ref())
            .and_then(Command::changes)
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
    /// their causal links but cannot supply the current value, the target or its preimage, and
    /// neither can an `unwritten` change, a write the reader holds inert: a revert never restores
    /// one, and a target none of whose writes projected has nothing to revert. Every reader
    /// refuses a revert that does not name each key of its target ([`Self::read_with`]), so a
    /// key where the target's write is inert, which it never changed, is restated as it reads
    /// now, while that register holds one value or none.
    pub(crate) fn compensate(
        &self,
        request: [u8; 16],
        target: Digest,
        excluded: &BTreeSet<Digest>,
        unwritten: &Unwritten,
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
        if self.is_settled(&target) {
            return Err(Error::new(SETTLED_REVERT_REFUSAL));
        }
        if !self.eligible.contains(&target) || excluded.contains(&target) {
            return Err(Error::new("target proposal is not eligible"));
        }
        let inert = |change: &Change| {
            unwritten.contains(&(target, change.collection.clone(), change.key.clone()))
        };
        if changes.iter().all(inert) {
            return Err(Error::new(
                "every write of the target proposal is inert; it never projected",
            ));
        }
        let mut current = self.project_cells(None, excluded, unwritten);
        // Per key: the register it restores, read now where the target's write is inert.
        let mut keys = Vec::with_capacity(changes.len());
        for change in changes {
            let key = (change.collection.clone(), change.key.clone());
            let now = current.get(&key);
            if inert(change) {
                if now.is_some_and(|cell| cell.stopped || cell.candidates.len() > 1) {
                    return Err(Error::new(
                        "a key where the target proposal's write is inert is stopped or conflicted; resolve it explicitly",
                    ));
                }
                let now = current.remove(&key);
                keys.push((key, Some(now)));
                continue;
            }
            if !now.is_some_and(|cell| {
                !cell.stopped
                    && cell.candidates.len() == 1
                    && cell.candidates[0].operation == target
            }) {
                return Err(Error::new(
                    "an affected proposal changed; resolve it before reverting",
                ));
            }
            keys.push((key, None));
        }
        let mut past = self.project_cells(Some(target), excluded, unwritten);
        let mut restored = Vec::with_capacity(keys.len());
        for (key, now) in keys {
            let old = match now {
                Some(now) => now,
                None => {
                    let old = past.remove(&key);
                    if old
                        .as_ref()
                        .is_some_and(|cell| cell.stopped || cell.candidates.len() > 1)
                    {
                        return Err(Error::new(
                            "previous proposal is stopped or conflicted; resolve it explicitly",
                        ));
                    }
                    old
                }
            };
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
            said: None,
        })
    }
}

#[cfg(test)]
#[path = "log_history_clock_tests.rs"]
mod history_clock_tests;
