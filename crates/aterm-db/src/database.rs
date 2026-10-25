// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic application views over authenticated causal history. Concurrent
//! values survive together; a resolving edit must actually observe the candidates.
//! Tombstones stop an object generation forever. Use a new key for a new generation.

use crate::{
    Error, Result,
    crypto::{self, Opened},
};
use astream_wire::sync::{Actor, Digest, Index, Projection};
use aterm_codec::hex;
use aterm_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Maximum retained operations in one initial database generation.
pub const MAX_RECORDS: usize = 20_000;
/// Largest local atomic write set.
pub const MAX_WRITES: usize = 64;

/// One key change within a locally atomic transaction. `None` is a permanent stop
/// in the private database and reversible absence in Commons account proposals.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// Named application collection.
    pub collection: String,
    /// Stable object-generation identifier.
    pub key: String,
    /// Whole value bundle, never a per-field last-writer merge.
    pub value: Option<Value>,
}

/// A typed application command inside the encrypted operation.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Generic collection transaction; Accounts applies a closed schema.
    Write {
        /// Durable retry identity supplied before the first attempt.
        request: [u8; 16],
        /// All changes become visible in one causal operation.
        changes: Vec<Change>,
    },
    /// An ordinary compensating edit. The referenced operation stays in history.
    /// An absent value here restores absence reversibly, unlike a permanent stop.
    Revert {
        /// Durable retry identity.
        request: [u8; 16],
        /// Exact operation being compensated, never a wall-clock position.
        operation: Digest,
        /// Whole-value compensations for the target transaction's keys.
        changes: Vec<Change>,
    },
    /// Immutable owner-authorized allocation, in application-defined admission units.
    Budget {
        /// Retry identity.
        request: [u8; 16],
        /// Unique window/generation name; never reused.
        name: String,
        /// Total units.
        total: u64,
        /// Fixed shares by certified actor and incarnation.
        shares: BTreeMap<Actor, u64>,
    },
    /// A writer's cumulative consumption under an exact observed allocation.
    Spend {
        /// Retry identity.
        request: [u8; 16],
        /// Content identity of its immutable budget definition.
        grant: Digest,
        /// Total consumed by this writer, including this request.
        cumulative: u64,
        /// This request's debit, preserved across retries.
        cost: u64,
    },
    /// Monotone catalog-write revocation. Old finite grant accounting remains
    /// admissible and its unreturned share is never reallocated by this decision.
    Revoke {
        /// Durable retry identity.
        request: [u8; 16],
        /// Exact writer installation being retired.
        actor: Actor,
        /// Last allowed record on its exact chain; None withholds all catalog edits.
        through: Option<Digest>,
    },
}
impl Command {
    /// Retry identity independent of transport acknowledgments.
    pub fn request(&self) -> [u8; 16] {
        match self {
            Self::Write { request, .. }
            | Self::Revert { request, .. }
            | Self::Budget { request, .. }
            | Self::Spend { request, .. } => *request,
            Self::Revoke { request, .. } => *request,
        }
    }
    /// A catalog transaction's keys and complete values, including compensations.
    pub fn changes(&self) -> Option<&[Change]> {
        match self {
            Self::Write { changes, .. } | Self::Revert { changes, .. } => Some(changes),
            _ => None,
        }
    }
    /// Decisions signed on the authority's independent hash chain.
    pub fn is_control(&self) -> bool {
        matches!(self, Self::Budget { .. } | Self::Revoke { .. })
            || self
                .changes()
                .is_some_and(|changes| changes.iter().any(|c| c.collection == "machines"))
    }
    /// Encode the closed versioned payload. All maps use the workspace's sorted map.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let value = match self {
            Self::Write { request, changes } => {
                json!({"v":1,"request":hex::encode_lower(request),"kind":"write","changes":changes.iter().map(|c|json!({"collection":c.collection,"key":c.key,"value":c.value})).collect::<Vec<_>>()})
            }
            Self::Revert {
                request,
                operation,
                changes,
            } => {
                json!({"v":1,"request":hex::encode_lower(request),"kind":"revert","operation":hex::encode_lower(operation),"changes":changes.iter().map(|c|json!({"collection":c.collection,"key":c.key,"value":c.value})).collect::<Vec<_>>()})
            }
            Self::Budget {
                request,
                name,
                total,
                shares,
            } => {
                let shares: aterm_json::Map = shares
                    .iter()
                    .map(|(a, n)| (hex::encode_lower(a), Value::from(*n)))
                    .collect();
                json!({"v":1,"request":hex::encode_lower(request),"kind":"budget","name":name,"total":total,"shares":shares})
            }
            Self::Spend {
                request,
                grant,
                cumulative,
                cost,
            } => {
                json!({"v":1,"request":hex::encode_lower(request),"kind":"spend","grant":hex::encode_lower(grant),"cumulative":cumulative,"cost":cost})
            }
            Self::Revoke {
                request,
                actor,
                through,
            } => {
                json!({"v":1,"request":hex::encode_lower(request),"kind":"revoke","actor":hex::encode_lower(actor),"through":through.as_ref().map(|d|hex::encode_lower(d))})
            }
        };
        aterm_json::to_vec(&value).map_err(|_| Error::new("database payload could not be encoded"))
    }
    /// Parse only the supported schema; unknown fields cannot silently change authority.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > crypto::MAX_BODY {
            return Err(Error::new("database payload exceeds limit"));
        }
        let v: Value = aterm_json::from_slice(bytes)
            .map_err(|_| Error::new("invalid database payload JSON"))?;
        if v.get("v").and_then(Value::as_u64) != Some(1) {
            return Err(Error::new("unsupported database application schema"));
        }
        let request = decode_hex(field(&v, "request")?)?;
        let command = match field(&v, "kind")? {
            "write" | "revert" => {
                let reverts = field(&v, "kind")? == "revert";
                if reverts {
                    keys(&v, &["v", "request", "kind", "changes", "operation"])?;
                } else {
                    keys(&v, &["v", "request", "kind", "changes"])?;
                }
                let raw = v
                    .get("changes")
                    .and_then(Value::as_array)
                    .filter(|c| !c.is_empty() && c.len() <= MAX_WRITES)
                    .ok_or_else(|| Error::new("invalid transaction write count"))?;
                let mut changes = Vec::new();
                let mut unique = BTreeSet::new();
                for c in raw {
                    keys(c, &["collection", "key", "value"])?;
                    let collection = field(c, "collection")?.to_owned();
                    let key = field(c, "key")?.to_owned();
                    if !crypto::name(&collection)
                        || collection.starts_with('_')
                        || !crypto::name(&key)
                        || !unique.insert((collection.clone(), key.clone()))
                    {
                        return Err(Error::new("invalid or duplicate transaction key"));
                    }
                    let value = c.get("value").filter(|v| !v.is_null()).cloned();
                    if collection == "accounts"
                        && let Some(v) = &value
                    {
                        crate::accounts::validate(v)?;
                    }
                    if let Some(v) = &value {
                        match collection.as_str() {
                            "machines" => crate::machines::validate(&key, v)?,
                            "mailboxes" | "email-addresses" | "phones" => {
                                crate::contacts::validate(&collection, v)?
                            }
                            _ => {}
                        }
                    }
                    changes.push(Change {
                        collection,
                        key,
                        value,
                    });
                }
                if changes.iter().any(|c| c.collection == "machines")
                    && changes.iter().any(|c| c.collection != "machines")
                {
                    return Err(Error::new(
                        "machine assignments cannot mix authority domains in one transaction",
                    ));
                }
                if reverts {
                    Self::Revert {
                        request,
                        operation: decode_hex(field(&v, "operation")?)?,
                        changes,
                    }
                } else {
                    Self::Write { request, changes }
                }
            }
            "budget" => {
                keys(&v, &["v", "request", "kind", "name", "total", "shares"])?;
                let name = field(&v, "name")?.to_owned();
                if !crypto::name(&name) {
                    return Err(Error::new("invalid budget generation name"));
                }
                let total = number(&v, "total")?;
                let raw = v
                    .get("shares")
                    .and_then(Value::as_object)
                    .filter(|s| !s.is_empty() && s.len() <= 64)
                    .ok_or_else(|| Error::new("invalid budget shares"))?;
                let mut sum = 0u64;
                let mut shares = BTreeMap::new();
                for (a, n) in raw {
                    let amount = n
                        .as_u64()
                        .ok_or_else(|| Error::new("invalid budget amount"))?;
                    sum = sum
                        .checked_add(amount)
                        .ok_or_else(|| Error::new("budget shares overflow"))?;
                    shares.insert(decode_hex(a)?, amount);
                }
                if sum > total {
                    return Err(Error::new("budget allocations exceed the total"));
                }
                Self::Budget {
                    request,
                    name,
                    total,
                    shares,
                }
            }
            "spend" => {
                keys(&v, &["v", "request", "kind", "grant", "cumulative", "cost"])?;
                Self::Spend {
                    request,
                    grant: decode_hex(field(&v, "grant")?)?,
                    cumulative: number(&v, "cumulative")?,
                    cost: number(&v, "cost")?,
                }
            }
            "revoke" => {
                keys(&v, &["v", "request", "kind", "actor", "through"])?;
                let actor = decode_hex(field(&v, "actor")?)?;
                let through = match v.get("through") {
                    Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(decode_hex(s)?),
                    _ => return Err(Error::new("invalid revocation cutoff")),
                };
                Self::Revoke {
                    request,
                    actor,
                    through,
                }
            }
            _ => return Err(Error::new("unknown database application command")),
        };
        Ok(command)
    }
}

/// One concurrent whole-value candidate, with provenance for a resolution.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// Signed operation identity.
    pub operation: Digest,
    /// Complete application value.
    pub value: Value,
}
/// An object view. More than one candidate is an explicit conflict.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    /// Stop facts never expire within a generation.
    pub stopped: bool,
    /// Concurrent candidate bundles.
    pub candidates: Vec<Candidate>,
}
/// Rebuilt local read snapshot; no arrival-time-dependent authority decisions.
#[derive(Clone, Debug, Default)]
pub struct View {
    /// Exact causal projection and quarantine evidence.
    pub causal: Projection,
    /// Application cells.
    pub cells: BTreeMap<(String, String), Cell>,
    /// Named budget definitions; multiple entries block spending.
    pub budgets: BTreeMap<String, Vec<Digest>>,
    /// Cumulative accepted accounting by exact grant and actor.
    pub spent: BTreeMap<(Digest, Actor), u64>,
    /// Authenticated commands that violated application authority.
    pub invalid: BTreeSet<Digest>,
    /// Every historical canonical or legacy name, including names superseded by renames.
    pub machine_names: BTreeMap<String, BTreeSet<String>>,
    /// All authenticated monotone catalog cutoffs, interpreted by exact hash ancestry.
    pub revocations: BTreeMap<Actor, Vec<Option<Digest>>>,
    /// A fork of an owner key blocks authority-bearing actions, not just one branch.
    pub authority_fault: bool,
    /// Conservative retained expenditure evidence, including forked or pending records.
    /// This is not a refund source and may exceed accepted accounting or the grant.
    pub liability: BTreeMap<(Digest, Actor), u64>,
    /// Shares whose signed history cannot currently establish safe further admission.
    pub disputed_shares: BTreeSet<(Digest, Actor)>,
}

/// Authenticated operation set and its reusable causal index. Persistence is in `local`.
#[derive(Clone)]
pub struct History {
    /// Rebuildable transport-independent astream synchronization index.
    pub index: Index,
    records: BTreeMap<Digest, (Opened, Command)>,
    refused: BTreeMap<Digest, (Opened, String)>,
    root: [u8; 32],
}
impl History {
    /// Empty history for this dataset's admission authority.
    pub fn new(root: [u8; 32]) -> Self {
        Self {
            index: Index::new(MAX_RECORDS),
            records: BTreeMap::new(),
            refused: BTreeMap::new(),
            root,
        }
    }
    /// Retain an authenticated causal record without applying side effects.
    /// Unsupported or unauthorized application payloads are inert evidence. They
    /// remain in inventories and fork detection, so one cannot poison every repair.
    pub fn insert(&mut self, opened: Opened) -> Result<bool> {
        let id = opened.header.id;
        if self.index.get(&id).is_some() {
            return Ok(false);
        }
        if !opened.control {
            let mut count = 0usize;
            let mut bytes = opened.wire.len();
            for old in self
                .records
                .values()
                .map(|(o, _)| o)
                .chain(self.refused.values().map(|(o, _)| o))
                .filter(|old| old.header.actor == opened.header.actor)
            {
                count += 1;
                bytes = bytes.saturating_add(old.wire.len());
            }
            if count >= 4096 || bytes > 8 * 1024 * 1024 {
                return Err(Error::new(
                    "writer's retained history quota is exhausted; preserve evidence and reconcile before admitting more",
                ));
            }
        }
        let decoded = Command::decode(&opened.plaintext).and_then(|command| {
            if command.is_control() != opened.control {
                return Err(Error::new(
                    "application command is signed under the wrong authority domain",
                ));
            }
            if let Command::Revoke { actor, .. } = &command
                && *actor == opened.header.actor
            {
                return Err(Error::new("authority stream cannot revoke itself"));
            }
            Ok(command)
        });
        let inserted = self
            .index
            .insert(opened.header.clone())
            .map_err(|_| Error::new("causal header refused or history capacity exhausted"))?;
        if inserted {
            match decoded {
                Ok(command) => {
                    self.records.insert(id, (opened, command));
                }
                Err(error) => {
                    self.refused.insert(id, (opened, error.0));
                }
            }
        }
        Ok(inserted)
    }
    /// Original ciphertext for inventory repair; includes pending and quarantined records.
    pub fn wire(&self, id: &Digest) -> Option<&[u8]> {
        self.records
            .get(id)
            .map(|(o, _)| o.wire.as_slice())
            .or_else(|| self.refused.get(id).map(|(o, _)| o.wire.as_slice()))
    }
    /// Bounded diagnostic reasons; never includes a rejected payload or key material.
    pub fn refusals(&self) -> impl Iterator<Item = (&Digest, &str)> {
        self.refused
            .iter()
            .map(|(id, (_, reason))| (id, reason.as_str()))
    }
    /// Signed author/time/causal metadata without decrypted application contents.
    /// Display names resolve through the machine registry rather than becoming identity.
    pub fn provenance(&self, id: &Digest) -> Option<Value> {
        let opened = self
            .records
            .get(id)
            .map(|(o, _)| o)
            .or_else(|| self.refused.get(id).map(|(o, _)| o))?;
        let projection = self.index.project();
        let reverts = self.command(id).and_then(|(_, c)| match c {
            Command::Revert { operation, .. } => Some(hex::encode_lower(operation)),
            _ => None,
        });
        Some(
            json!({"operation":hex::encode_lower(id),"writer_key":hex::encode_lower(&opened.header.actor[..32]),"writer_incarnation":hex::encode_lower(&opened.header.actor[32..]),"sequence":opened.header.sequence,"previous":opened.header.previous.as_ref().map(|id|hex::encode_lower(id)),"parents":opened.header.parents.iter().map(|id|hex::encode_lower(id)).collect::<Vec<_>>(),"authored_at_unix_ms":opened.authored_at_unix_ms,"time_source":"signed-author-claim","request":self.command(id).map(|(_,c)|hex::encode_lower(&c.request())),"reverts":reverts,"refused":self.refused.get(id).map(|(_,why)|why.as_str()),"quarantined":projection.quarantined.contains(&opened.header.actor),"causally_ready":projection.ordered.contains(id)}),
        )
    }
    /// Exact command and author, for typed budget and retry handling.
    pub fn command(&self, id: &Digest) -> Option<(&Actor, &Command)> {
        self.records.get(id).map(|(o, c)| (&o.header.actor, c))
    }
    /// Pending/quarantined allocations remain liabilities; withdrawing a view
    /// must never make their resource/window names available for fresh minting.
    pub fn has_budget(&self, name: &str) -> bool {
        self.records
            .values()
            .any(|(_, c)| matches!(c,Command::Budget{name:old,..}if old==name))
    }
    /// Full signed assignment history, including superseded names and disputed evidence.
    pub fn machine_history(&self, key: &str) -> Vec<Value> {
        let mut assignments = Vec::new();
        for (opened, command) in self.records.values() {
            if let Some(changes) = command.changes() {
                for change in changes
                    .iter()
                    .filter(|change| change.collection == "machines" && change.key == key)
                {
                    assignments.push((opened.header.sequence,opened.header.id,json!({"operation":hex::encode_lower(&opened.header.id),"author":hex::encode_lower(&opened.header.actor),"sequence":opened.header.sequence,"authored_at_unix_ms":opened.authored_at_unix_ms,"value":change.value})));
                }
            }
        }
        assignments.sort_by_key(|(sequence, id, _)| (*sequence, *id));
        assignments.into_iter().map(|(_, _, row)| row).collect()
    }
    /// Reconstruct the causal past of an exact operation. Concurrent changes are
    /// deliberately absent; they are checked separately against current heads.
    pub fn before(&self, id: &Digest) -> Result<View> {
        if self.index.get(id).is_none() {
            return Err(Error::new("operation is not retained"));
        }
        let mut past = Self::new(self.root);
        for prior in self
            .index
            .inventory()
            .filter(|prior| *prior != id && self.index.observes(id, prior))
        {
            let opened = self
                .records
                .get(prior)
                .map(|(o, _)| o)
                .or_else(|| self.refused.get(prior).map(|(o, _)| o))
                .ok_or_else(|| Error::new("causal record has no retained ciphertext"))?;
            past.insert(opened.clone())?;
        }
        Ok(past.view())
    }
    /// Find a durable command after a lost acknowledgement.
    pub fn find_request(&self, actor: &Actor, request: [u8; 16]) -> Option<(Digest, &Command)> {
        self.records.iter().find_map(|(id, (o, c))| {
            (&o.header.actor == actor && c.request() == request).then_some((*id, c))
        })
    }
    /// A command retry must return its original operation; differing content refuses.
    pub fn retry(&self, actor: &Actor, command: &Command) -> Result<Option<Digest>> {
        let mut found = None;
        for (id, (o, c)) in &self.records {
            if &o.header.actor == actor && c.request() == command.request() {
                if c != command {
                    return Err(Error::new(
                        "request identity was reused with different operations",
                    ));
                }
                let candidate = (o.header.sequence, *id);
                if found.is_none_or(|old| candidate < old) {
                    found = Some(candidate);
                }
            }
        }
        Ok(found.map(|(_, id)| id))
    }
    /// Pure deterministic view. Invalid accounting never creates usable capacity.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCausalVisibility",
            action = "ReceiveParent",
            project = "aterm_db::tests::visible_state"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCausalVisibility",
            action = "ReceiveChild",
            project = "aterm_db::tests::visible_state"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbCausalVisibility",
            action = "ReceiveFork",
            project = "aterm_db::tests::visible_state"
        )
    )]
    pub fn view(&self) -> View {
        let evidence = self.index.project();
        let mut authority_fault = evidence
            .quarantined
            .iter()
            .any(|actor| actor[..32] == self.root)
            || self.refused.values().any(|(opened, _)| opened.control);
        let mut requests = BTreeMap::<(Actor, [u8; 16]), Vec<Digest>>::new();
        for (id, (opened, command)) in &self.records {
            requests
                .entry((opened.header.actor, command.request()))
                .or_default()
                .push(*id);
        }
        let mut duplicate_effects = BTreeSet::new();
        let mut request_conflicts = BTreeSet::new();
        for ids in requests.values_mut() {
            ids.sort_by_key(|id| self.index.get(id).map(|h| (h.sequence, h.id)));
            if let Some(first) = ids.first() {
                if ids.iter().any(|id| {
                    self.command(id).map(|(_, c)| c) != self.command(first).map(|(_, c)| c)
                }) {
                    request_conflicts.extend(ids.iter().copied());
                    authority_fault |= ids
                        .iter()
                        .any(|id| self.records.get(id).is_some_and(|(o, _)| o.control));
                } else {
                    duplicate_effects.extend(ids.iter().skip(1).copied());
                }
            }
        }
        let mut revocations = BTreeMap::<Actor, Vec<Option<Digest>>>::new();
        for id in &evidence.ordered {
            if let Some((_, Command::Revoke { actor, through, .. })) = self.records.get(id) {
                revocations.entry(*actor).or_default().push(*through);
            }
        }
        let excluded = self
            .records
            .iter()
            .filter_map(|(id, (opened, command))| {
                let denied = authority_fault
                    || request_conflicts.contains(id)
                    || (command.changes().is_some()
                        && revocations.get(&opened.header.actor).is_some_and(|cuts| {
                            !cuts.iter().all(|cut| {
                                cut.is_some_and(|tip| {
                                    self.index
                                        .get(&tip)
                                        .is_some_and(|head| head.actor == opened.header.actor)
                                        && self.index.observes(&tip, id)
                                })
                            })
                        }));
                denied.then_some(*id)
            })
            .collect();
        let causal = self.index.project_excluding(&excluded);
        let eligible: BTreeSet<_> = causal.ordered.iter().copied().collect();
        let mut view = View {
            causal,
            revocations,
            authority_fault,
            ..View::default()
        };
        view.invalid.extend(self.refused.keys().copied());
        view.invalid.extend(request_conflicts);
        // Resource evidence does not disappear when a catalog projection withdraws
        // an actor. Count every distinct signed debit conservatively; a fork may
        // have launched different external work on both branches.
        let mut debit_sums = BTreeMap::<(Digest, Actor), u64>::new();
        for (id, (opened, command)) in &self.records {
            match command {
                Command::Budget { name, .. } => {
                    if !duplicate_effects.contains(id) {
                        view.budgets.entry(name.clone()).or_default().push(*id);
                    }
                }
                Command::Spend {
                    grant,
                    cumulative,
                    cost,
                    ..
                } => {
                    let share = (*grant, opened.header.actor);
                    let known = view.liability.entry(share).or_default();
                    *known = (*known).max(*cumulative);
                    let sum = debit_sums.entry(share).or_default();
                    *sum = sum.saturating_add(*cost);
                    if !evidence.ordered.contains(id)
                        || authority_fault
                        || view.invalid.contains(id)
                    {
                        view.disputed_shares.insert(share);
                    }
                }
                _ => {}
            }
        }
        for (share, sum) in debit_sums {
            if let Some(known) = view.liability.get_mut(&share) {
                *known = (*known).max(sum);
            }
        }
        let refused_actors: BTreeSet<_> = self
            .refused
            .values()
            .map(|(opened, _)| opened.header.actor)
            .collect();
        for (grant, (_, command)) in &self.records {
            if let Command::Budget { shares, .. } = command {
                for actor in shares
                    .keys()
                    .filter(|actor| refused_actors.contains(*actor))
                {
                    view.disputed_shares.insert((*grant, *actor));
                }
            }
        }
        for id in &evidence.ordered {
            if duplicate_effects.contains(id) {
                continue;
            }
            let Some((opened, command)) = self.records.get(id) else {
                continue;
            };
            match command {
                Command::Write { changes, .. } | Command::Revert { changes, .. } => {
                    if !eligible.contains(id) {
                        continue;
                    }
                    let reverting = matches!(command, Command::Revert { .. });
                    if let Command::Revert { operation, .. } = command
                        && (!self.index.observes(id, operation)
                            || self
                                .command(operation)
                                .and_then(|(_, c)| c.changes())
                                .is_none())
                    {
                        view.invalid.insert(*id);
                        continue;
                    }
                    for change in changes {
                        if change.collection == "machines"
                            && let Some(value) = &change.value
                        {
                            for name in crate::machines::names(value) {
                                view.machine_names
                                    .entry(name)
                                    .or_default()
                                    .insert(change.key.clone());
                            }
                        }
                    }
                    crate::documents::apply(&self.index, &mut view.cells, *id, changes, reverting);
                }
                Command::Budget { .. } => {}
                Command::Spend {
                    grant,
                    cumulative,
                    cost,
                    ..
                } => {
                    if view.invalid.contains(id) {
                        view.disputed_shares.insert((*grant, opened.header.actor));
                        continue;
                    }
                    let limit = match self.records.get(grant) {
                        Some((_, Command::Budget { name, shares, .. }))
                            if self.index.observes(id, grant)
                                && evidence.ordered.contains(grant)
                                && view.budgets.get(name).is_some_and(|g| g.len() == 1) =>
                        {
                            shares.get(&opened.header.actor).copied()
                        }
                        _ => None,
                    };
                    let previous = view
                        .spent
                        .get(&(*grant, opened.header.actor))
                        .copied()
                        .unwrap_or(0);
                    if limit.is_some_and(|limit| {
                        aterm_db_core::admissible(limit, previous, *cumulative)
                            && previous.checked_add(*cost) == Some(*cumulative)
                    }) {
                        view.spent.insert(
                            (*grant, opened.header.actor),
                            aterm_db_core::merge_spent(previous, *cumulative),
                        );
                    } else {
                        view.invalid.insert(*id);
                        view.disputed_shares.insert((*grant, opened.header.actor));
                    }
                }
                Command::Revoke { .. } => {}
            }
        }
        view
    }
}
pub(crate) fn field<'a>(v: &'a Value, name: &str) -> Result<&'a str> {
    v.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("database payload lacks a required string"))
}
fn number(v: &Value, name: &str) -> Result<u64> {
    v.get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::new("database payload lacks a required unsigned number"))
}
fn keys(v: &Value, keys: &[&str]) -> Result<()> {
    if v.as_object()
        .is_some_and(|o| o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k)))
    {
        Ok(())
    } else {
        Err(Error::new("database payload has unknown or missing fields"))
    }
}
/// Decode a bounded, canonical lowercase hex identifier.
pub fn decode_hex<const N: usize>(text: &str) -> Result<[u8; N]> {
    if text.len() != N * 2
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::new("invalid database hex identifier"));
    }
    let mut out = [0; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = (hex::nibble(text.as_bytes()[i * 2]).ok_or_else(|| Error::new("invalid hex"))?
            << 4)
            | hex::nibble(text.as_bytes()[i * 2 + 1]).ok_or_else(|| Error::new("invalid hex"))?;
    }
    Ok(out)
}
