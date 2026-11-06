// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic application views over authenticated causal history. Concurrent
//! values survive together; a resolving edit must actually observe the candidates.
//! Tombstones stop an object generation forever. Use a new key for a new generation.

use crate::{
    Error, Result,
    crypto::{self, Opened},
};
use astream_wire::sync::{Actor, Digest, Header, Index, Projection, sequence_follows};
use aterm_codec::hex;
use aterm_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Maximum retained operations in one initial database generation.
pub const MAX_RECORDS: usize = 20_000;
/// Largest local atomic write set.
pub const MAX_WRITES: usize = 64;
/// Most retained operations one writer may hold in a generation.
pub const MAX_WRITER_OPERATIONS: usize = 4096;
/// Most retained ciphertext one writer may hold in a generation.
pub const MAX_WRITER_BYTES: usize = 8 * 1024 * 1024;

/// One key change within a locally atomic transaction. `None` is a permanent stop
/// in the private database and reversible absence in Commons account proposals.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
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
    /// Join causal history without changing values, authority or resource accounting.
    Merge {
        /// Durable identity of this merge record, separate from the user's edit.
        request: [u8; 16],
    },
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
            Self::Merge { request }
            | Self::Write { request, .. }
            | Self::Revert { request, .. }
            | Self::Budget { request, .. }
            | Self::Spend { request, .. } => *request,
            Self::Revoke { request, .. } => *request,
        }
    }
    /// A catalog transaction's keys and complete values, including compensations.
    /// A causal merge is a catalog operation with an empty change list.
    pub fn changes(&self) -> Option<&[Change]> {
        match self {
            Self::Merge { .. } => Some(&[]),
            Self::Write { changes, .. } | Self::Revert { changes, .. } => Some(changes),
            _ => None,
        }
    }
    /// Whether this write stops a key forever rather than giving it a value.
    pub(crate) fn stops(&self) -> bool {
        matches!(self, Self::Write { changes, .. } if changes.iter().any(|c| c.value.is_none()))
    }
    /// Consume the command and return its cell changes if it is a `Write` or `Revert`.
    pub fn into_changes(self) -> Option<Vec<Change>> {
        match self {
            Self::Write { changes, .. } | Self::Revert { changes, .. } => Some(changes),
            _ => None,
        }
    }
    /// Encode a `Write` command payload without taking ownership of `changes`.
    pub fn encode_write(request: [u8; 16], changes: &[Change]) -> Result<Vec<u8>> {
        #[derive(serde::Serialize)]
        struct Encoded<'a> {
            changes: &'a [Change],
            kind: &'static str,
            request: &'a str,
            v: u8,
        }
        let request_hex = hex::encode_lower(&request);
        aterm_json::to_vec(&Encoded {
            changes,
            kind: "write",
            request: &request_hex,
            v: 1,
        })
        .map_err(|e| Error::new(&format!("command encode failed: {e}")))
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
        let request_hex = hex::encode_lower(&self.request());
        let encoded = match self {
            Self::Merge { .. } => {
                #[derive(serde::Serialize)]
                struct Encoded<'a> {
                    kind: &'static str,
                    request: &'a str,
                    v: u8,
                }
                aterm_json::to_vec(&Encoded {
                    kind: "merge",
                    request: &request_hex,
                    v: 1,
                })
            }
            Self::Write { request, changes } => return Self::encode_write(*request, changes),
            Self::Revert {
                operation, changes, ..
            } => {
                #[derive(serde::Serialize)]
                struct Encoded<'a> {
                    changes: &'a [Change],
                    kind: &'static str,
                    operation: &'a str,
                    request: &'a str,
                    v: u8,
                }
                let op_hex = hex::encode_lower(operation);
                aterm_json::to_vec(&Encoded {
                    changes,
                    kind: "revert",
                    operation: &op_hex,
                    request: &request_hex,
                    v: 1,
                })
            }
            Self::Budget {
                name,
                total,
                shares,
                ..
            } => {
                #[derive(serde::Serialize)]
                struct Encoded<'a> {
                    kind: &'static str,
                    name: &'a str,
                    request: &'a str,
                    shares: BTreeMap<String, u64>,
                    total: u64,
                    v: u8,
                }
                let shares: BTreeMap<String, u64> = shares
                    .iter()
                    .map(|(a, n)| (hex::encode_lower(a), *n))
                    .collect();
                aterm_json::to_vec(&Encoded {
                    kind: "budget",
                    name,
                    request: &request_hex,
                    shares,
                    total: *total,
                    v: 1,
                })
            }
            Self::Spend {
                grant,
                cumulative,
                cost,
                ..
            } => {
                #[derive(serde::Serialize)]
                struct Encoded<'a> {
                    cost: u64,
                    cumulative: u64,
                    grant: &'a str,
                    kind: &'static str,
                    request: &'a str,
                    v: u8,
                }
                let grant_hex = hex::encode_lower(grant);
                aterm_json::to_vec(&Encoded {
                    cost: *cost,
                    cumulative: *cumulative,
                    grant: &grant_hex,
                    kind: "spend",
                    request: &request_hex,
                    v: 1,
                })
            }
            Self::Revoke { actor, through, .. } => {
                #[derive(serde::Serialize)]
                struct Encoded<'a> {
                    actor: &'a str,
                    kind: &'static str,
                    request: &'a str,
                    through: Option<&'a str>,
                    v: u8,
                }
                let actor_hex = hex::encode_lower(actor);
                let through_hex = through.as_ref().map(|d| hex::encode_lower(d));
                aterm_json::to_vec(&Encoded {
                    actor: &actor_hex,
                    kind: "revoke",
                    request: &request_hex,
                    through: through_hex.as_deref(),
                    v: 1,
                })
            }
        };
        encoded.map_err(|_| Error::new("database payload could not be encoded"))
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
            "merge" => {
                keys(&v, &["v", "request", "kind"])?;
                Self::Merge { request }
            }
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
                let mut changes = Vec::with_capacity(raw.len());
                let mut unique = BTreeSet::new();
                for c in raw {
                    keys(c, &["collection", "key", "value"])?;
                    let collection = field(c, "collection")?;
                    let key = field(c, "key")?;
                    if !crypto::name(collection)
                        || collection.starts_with('_')
                        || !crypto::name(key)
                        || !unique.insert((collection, key))
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
                        match collection {
                            "machines" => crate::machines::validate(key, v)?,
                            "emails" => crate::fleet_db::validate_email_record(key, v)?,
                            "mailboxes" | "email-addresses" | "phones" => {
                                crate::contacts::validate(collection, v)?
                            }
                            _ => {}
                        }
                    }
                    changes.push(Change {
                        collection: collection.to_owned(),
                        key: key.to_owned(),
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
    no_authority: bool,
}
impl History {
    /// Empty history for this dataset's admission authority.
    pub fn new(root: [u8; 32]) -> Self {
        Self {
            index: Index::new(MAX_RECORDS),
            records: BTreeMap::new(),
            refused: BTreeMap::new(),
            root,
            no_authority: false,
        }
    }
    /// Empty history for a dataset with no authority stream: every member can derive its root
    /// key, so a control certificate proves nothing and [`History::insert`] keeps one as inert
    /// evidence, a causal node what a replica wrote after seeing it can still rest on.
    pub(crate) fn without_authority(root: [u8; 32]) -> Self {
        Self {
            no_authority: true,
            ..Self::new(root)
        }
    }
    /// The operations and ciphertext bytes retained for `actor`, which its quotas bound.
    pub(crate) fn writer_usage(&self, actor: &Actor) -> (usize, usize) {
        self.records
            .values()
            .map(|(o, _)| o)
            .chain(self.refused.values().map(|(o, _)| o))
            .filter(|old| old.header.actor == *actor)
            .fold((0, 0), |(count, bytes), old| {
                (count + 1, usize::saturating_add(bytes, old.wire.len()))
            })
    }
    /// Retain an authenticated causal record without applying side effects.
    /// Unsupported or unauthorized application payloads are inert evidence. They
    /// remain in inventories and fork detection, so one cannot poison every repair.
    pub fn insert(&mut self, opened: Opened) -> Result<bool> {
        if !self.check_insert(&opened)? {
            return Ok(false);
        }
        self.retain(opened)
    }
    pub(crate) fn check_insert(&self, opened: &Opened) -> Result<bool> {
        let id = opened.header.id;
        if self.index.get(&id).is_some() {
            return Ok(false);
        }
        if !opened.control || self.no_authority {
            let (count, bytes) = self.writer_usage(&opened.header.actor);
            if count >= MAX_WRITER_OPERATIONS
                || bytes.saturating_add(opened.wire.len()) > MAX_WRITER_BYTES
            {
                return Err(Error::new(
                    "writer's retained history quota is exhausted; preserve evidence and reconcile before admitting more",
                ));
            }
        }
        if self.records.len().saturating_add(self.refused.len()) >= MAX_RECORDS
            || Index::new(1).insert(opened.header.clone()).is_err()
        {
            return Err(Error::new(
                "causal header refused or history capacity exhausted",
            ));
        }
        Ok(true)
    }
    /// A record already in this replica's journal. An earlier build kept a control record outside
    /// the writer quotas, so counting it now would refuse the whole folder, its rekey included.
    pub(crate) fn replay(&mut self, opened: Opened) -> Result<bool> {
        if opened.control {
            self.retain(opened)
        } else {
            self.insert(opened)
        }
    }
    pub(crate) fn retain(&mut self, opened: Opened) -> Result<bool> {
        let id = opened.header.id;
        let inserted = self
            .index
            .insert(opened.header.clone())
            .map_err(|_| Error::new("causal header refused or history capacity exhausted"))?;
        if !inserted {
            return Ok(false);
        }
        let decoded = Command::decode(&opened.plaintext).and_then(|command| {
            if opened.control && self.no_authority {
                return Err(Error::new(
                    "this dataset has no authority stream; a control record has no effect in it",
                ));
            }
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
            // A stop cannot be undone and every member can sign one: with no authority stream it
            // would hide a row for good on every replica, so it stays inert evidence.
            if self.no_authority && command.stops() {
                return Err(Error::new(
                    "this dataset has no authority stream; a permanent stop has no effect in it",
                ));
            }
            Ok(command)
        });
        match decoded {
            Ok(command) => {
                self.records.insert(id, (opened, command));
            }
            Err(error) => {
                self.refused.insert(id, (opened, error.0));
            }
        }
        Ok(true)
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
    pub(crate) fn is_refused(&self, id: &Digest) -> bool {
        self.refused.contains_key(id)
    }
    /// Signed author/time/causal metadata without decrypted application contents.
    /// Display names resolve through the machine registry rather than becoming identity.
    pub fn provenance(&self, id: &Digest) -> Option<Value> {
        let projection = self.project(&BTreeSet::new());
        self.provenance_with(id, &projection.quarantined, |id| {
            projection.ordered.contains(id)
        })
    }

    pub(crate) fn provenance_with(
        &self,
        id: &Digest,
        quarantined: &BTreeSet<Actor>,
        is_ordered: impl FnOnce(&Digest) -> bool,
    ) -> Option<Value> {
        let opened = self
            .records
            .get(id)
            .map(|(o, _)| o)
            .or_else(|| self.refused.get(id).map(|(o, _)| o))?;
        let command = self.command(id).map(|(_, c)| c);
        let reverts = command.and_then(|c| match c {
            Command::Revert { operation, .. } => Some(hex::encode_lower(operation)),
            _ => None,
        });
        Some(
            json!({"operation":hex::encode_lower(id),"writer_key":hex::encode_lower(&opened.header.actor[..32]),"writer_incarnation":hex::encode_lower(&opened.header.actor[32..]),"sequence":opened.header.sequence,"previous":opened.header.previous.as_ref().map(|id|hex::encode_lower(id)),"parents":opened.header.parents.iter().map(|id|hex::encode_lower(id)).collect::<Vec<_>>(),"authored_at_unix_ms":opened.authored_at_unix_ms,"time_source":"signed-author-claim","request":command.map(|c|hex::encode_lower(&c.request())),"reverts":reverts,"refused":self.refused.get(id).map(|(_,why)|why.as_str()),"quarantined":quarantined.contains(&opened.header.actor),"causally_ready":is_ordered(id)}),
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
        assignments.sort_unstable_by_key(|(sequence, id, _)| (*sequence, *id));
        assignments.into_iter().map(|(_, _, row)| row).collect()
    }
    /// Reconstruct the causal past of an exact operation. Concurrent changes are
    /// deliberately absent; they are checked separately against current heads.
    pub fn before(&self, id: &Digest) -> Result<View> {
        if self.index.get(id).is_none() {
            return Err(Error::new("operation is not retained"));
        }
        let mut past = Self {
            no_authority: self.no_authority,
            ..Self::new(self.root)
        };
        for prior in self
            .index
            .inventory()
            .filter(|prior| *prior != id && self.index.observes(id, prior))
        {
            if let Some((opened, command)) = self.records.get(prior) {
                past.index.insert(opened.header.clone()).map_err(|_| {
                    Error::new("causal header refused or history capacity exhausted")
                })?;
                past.records
                    .insert(*prior, (opened.clone(), command.clone()));
            } else if let Some((opened, reason)) = self.refused.get(prior) {
                past.index.insert(opened.header.clone()).map_err(|_| {
                    Error::new("causal header refused or history capacity exhausted")
                })?;
                past.refused
                    .insert(*prior, (opened.clone(), reason.clone()));
            } else {
                return Err(Error::new("causal record has no retained ciphertext"));
            }
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
    /// The causal projection. Where every member can mint a writer, one member's fork must not
    /// withhold what the others wrote after seeing it.
    pub(crate) fn project(&self, excluded: &BTreeSet<Digest>) -> Projection {
        if self.no_authority {
            self.project_own_effects(excluded)
        } else {
            self.index.project_excluding(excluded)
        }
    }
    /// [`Index::project_excluding`] for a dataset with no authority stream. A forked writer loses
    /// only its records from the first disputed sequence on, the ones its two branches disagree
    /// about, and an excluded record only itself. A record that named a withheld one as a parent
    /// stays visible, ordered after what the withheld record depended on. So a folder restored
    /// from a backup, which then writes the sequences it wrote before, costs what it wrote since.
    fn project_own_effects(&self, excluded: &BTreeSet<Digest>) -> Projection {
        let headers: BTreeMap<&Digest, &Header> = self
            .index
            .inventory()
            .filter_map(|id| self.index.get(id).map(|header| (id, header)))
            .collect();
        let mut claims = BTreeMap::<(Actor, u64), Digest>::new();
        let mut disputed = BTreeMap::<Actor, u64>::new();
        for header in headers.values() {
            let earlier = header.previous.map(|id| (header.sequence - 1, id));
            for (sequence, id) in std::iter::once((header.sequence, header.id)).chain(earlier) {
                if *claims.entry((header.actor, sequence)).or_insert(id) != id {
                    let from = disputed.entry(header.actor).or_insert(sequence);
                    *from = (*from).min(sequence);
                }
            }
        }
        let withheld = |header: &Header| {
            excluded.contains(&header.id)
                || disputed
                    .get(&header.actor)
                    .is_some_and(|from| header.sequence >= *from)
        };
        let mut out = Projection {
            quarantined: disputed.keys().copied().collect(),
            ..Projection::default()
        };
        let mut waiting = BTreeMap::<Digest, usize>::new();
        let mut children = BTreeMap::<Digest, Vec<Digest>>::new();
        let mut edges = BTreeMap::<Digest, BTreeSet<Digest>>::new();
        let mut ready = BTreeSet::new();
        for header in headers.values() {
            out.pending.insert(header.id);
            if withheld(header)
                || header.previous.is_some_and(|previous| {
                    !headers.get(&previous).is_some_and(|parent| {
                        parent.actor == header.actor
                            && sequence_follows(parent.sequence, header.sequence)
                    })
                })
            {
                continue;
            }
            let mut parents = BTreeSet::new();
            let mut walked = BTreeSet::new();
            let mut todo = header.parents.clone();
            while let Some(parent) = todo.pop() {
                if !walked.insert(parent) {
                    continue;
                }
                match headers.get(&parent) {
                    Some(hidden) if withheld(hidden) => todo.extend(&hidden.parents),
                    _ => {
                        parents.insert(parent);
                    }
                }
            }
            waiting.insert(header.id, parents.len());
            if parents.is_empty() {
                ready.insert(header.id);
            }
            for parent in &parents {
                children.entry(*parent).or_default().push(header.id);
            }
            edges.insert(header.id, parents);
        }
        while let Some(id) = ready.pop_first() {
            out.pending.remove(&id);
            out.ordered.push(id);
            out.heads.insert(id);
            for parent in edges.get(&id).into_iter().flatten() {
                out.heads.remove(parent);
            }
            for child in children.get(&id).into_iter().flatten() {
                if let Some(count) = waiting.get_mut(child) {
                    *count -= 1;
                    if *count == 0 {
                        ready.insert(*child);
                    }
                }
            }
        }
        out
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
        let evidence = self.project(&BTreeSet::new());
        let mut authority_fault = !self.no_authority
            && (evidence
                .quarantined
                .iter()
                .any(|actor| actor[..32] == self.root)
                || self.refused.values().any(|(opened, _)| opened.control));
        enum SeenRequest {
            One(Digest),
            Many(Vec<Digest>),
        }
        let mut requests = BTreeMap::<(Actor, [u8; 16]), SeenRequest>::new();
        for (id, (opened, command)) in &self.records {
            match requests.entry((opened.header.actor, command.request())) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(SeenRequest::One(*id));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => match entry.get_mut() {
                    SeenRequest::One(first) => {
                        let first = *first;
                        entry.insert(SeenRequest::Many(vec![first, *id]));
                    }
                    SeenRequest::Many(ids) => ids.push(*id),
                },
            }
        }
        let mut duplicate_effects = BTreeSet::new();
        let mut request_conflicts = BTreeSet::new();
        for entry in requests.values_mut() {
            let SeenRequest::Many(ids) = entry else {
                continue;
            };
            ids.sort_unstable_by_key(|id| self.index.get(id).map(|h| (h.sequence, h.id)));
            if let Some(first) = ids.first() {
                let first_cmd = self.command(first).map(|(_, c)| c);
                if ids[1..]
                    .iter()
                    .any(|id| self.command(id).map(|(_, c)| c) != first_cmd)
                {
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
        let excluded: BTreeSet<Digest> =
            if !authority_fault && request_conflicts.is_empty() && revocations.is_empty() {
                BTreeSet::new()
            } else {
                self.records
                    .iter()
                    .filter_map(|(id, (opened, command))| {
                        let denied = authority_fault
                            || request_conflicts.contains(id)
                            || (command.changes().is_some()
                                && revocations.get(&opened.header.actor).is_some_and(|cuts| {
                                    !cuts.iter().all(|cut| {
                                        cut.is_some_and(|tip| {
                                            self.index.get(&tip).is_some_and(|head| {
                                                head.actor == opened.header.actor
                                            }) && self.index.observes(&tip, id)
                                        })
                                    })
                                }));
                        denied.then_some(*id)
                    })
                    .collect()
            };
        let causal = (!excluded.is_empty()).then(|| self.project(&excluded));
        let eligible_override: Option<BTreeSet<_>> =
            causal.as_ref().map(|c| c.ordered.iter().copied().collect());
        let evidence_ordered: BTreeSet<_> = evidence.ordered.iter().copied().collect();
        let eligible = eligible_override.as_ref().unwrap_or(&evidence_ordered);
        let mut view = View {
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
                    if !evidence_ordered.contains(id)
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
                // Retaining an allocation reserves its name, but cannot authorize
                // work before its required authority history is projected.
                let grant_eligible = evidence_ordered.contains(grant);
                for actor in shares
                    .keys()
                    .filter(|actor| !grant_eligible || refused_actors.contains(*actor))
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
                        && (id == operation
                            || !self.index.observes(id, operation)
                            || !eligible.contains(operation)
                            || view.invalid.contains(operation)
                            || !self.command(operation).is_some_and(|(_, c)| match c {
                                Command::Write {
                                    changes: target, ..
                                } => {
                                    target.iter().all(|c| c.value.is_some())
                                        && same_change_keys(target, changes)
                                }
                                Command::Revert {
                                    changes: target, ..
                                } => same_change_keys(target, changes),
                                _ => false,
                            }))
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
                Command::Merge { .. } | Command::Budget { .. } => {}
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
                                && evidence_ordered.contains(grant)
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
        view.causal = causal.unwrap_or(evidence);
        view
    }
}
pub(crate) fn same_change_keys(a: &[Change], b: &[Change]) -> bool {
    if a.len() == 1 && b.len() == 1 {
        return a[0].collection == b[0].collection && a[0].key == b[0].key;
    }
    if a.len() <= 8 && b.len() <= 8 {
        return a.iter().all(|x| {
            b.iter()
                .any(|y| x.collection == y.collection && x.key == y.key)
        }) && b.iter().all(|y| {
            a.iter()
                .any(|x| x.collection == y.collection && x.key == y.key)
        });
    }
    change_keys(a) == change_keys(b)
}
pub(crate) fn change_keys(items: &[Change]) -> BTreeSet<(&str, &str)> {
    items
        .iter()
        .map(|c| (c.collection.as_str(), c.key.as_str()))
        .collect()
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
    if text.len() != N * 2 {
        return Err(Error::new("invalid database hex identifier"));
    }
    let lower_nibble = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let mut out = [0; N];
    for (byte, pair) in out.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
        let high =
            lower_nibble(pair[0]).ok_or_else(|| Error::new("invalid database hex identifier"))?;
        let low =
            lower_nibble(pair[1]).ok_or_else(|| Error::new("invalid database hex identifier"))?;
        *byte = (high << 4) | low;
    }
    Ok(out)
}
