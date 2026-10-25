// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private local identities, explicit peer invitations and strict astream storage.
//! Acknowledgement follows durable append; derived indexes are rebuilt from ciphertext.

use crate::{
    Error, Result,
    crypto::{self, Capability, Manifest},
    database::{Change, Command, History, View, decode_hex, field},
};
use astream_broker::{BrokerLog, Durability};
use astream_wire::{
    Offset,
    sync::{Actor, Digest},
};
use aterm_codec::hex;
use aterm_json::{Value, json};
use aterm_uds::private_file::{self, PrivateFileError};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

const CONFIG: &str = "database.json";
const IDENTITY: &str = "identity";
const MAX_JOURNAL: usize = 128 * 1024 * 1024;

/// Dataset keys and a local installation identity. Never formatted or printed.
pub struct Credentials {
    /// Verified immutable dataset descriptor.
    pub manifest: Manifest,
    /// Root-signed admission for this local identity.
    pub capability: Capability,
    pub(crate) seed: [u8; 32],
    pub(crate) key: [u8; 32],
    root_seed: Option<[u8; 32]>,
}
impl Credentials {
    /// Load private bounded state and prove its signing identity matches its admission.
    pub fn load(dir: &Path) -> Result<Self> {
        let bytes = private_file::read_private(dir, CONFIG, 4096)
            .map_err(|_| Error::new("database configuration missing or not private"))?;
        let v: Value = aterm_json::from_slice(&bytes)
            .map_err(|_| Error::new("invalid database configuration"))?;
        let manifest = Manifest::decode(&unhex(field(&v, "manifest")?, 256)?)?;
        let capability = Capability::decode(&manifest, &unhex(field(&v, "capability")?, 256)?)?;
        let (seed, actor) = identity(dir)?;
        if actor != capability.actor {
            return Err(Error::new(
                "database admission belongs to another installation",
            ));
        }
        let key = decode_hex(field(&v, "key")?)?;
        let root_seed = v
            .get("root_seed")
            .and_then(Value::as_str)
            .map(decode_hex)
            .transpose()?;
        if root_seed
            .as_ref()
            .is_some_and(|s| crypto::public(s).ok() != Some(manifest.root))
        {
            return Err(Error::new(
                "database authority key differs from its descriptor",
            ));
        }
        Ok(Self {
            manifest,
            capability,
            seed,
            key,
            root_seed,
        })
    }
    fn save(&self, dir: &Path) -> Result<()> {
        let v = json!({"manifest":hex::encode_lower(self.manifest.bytes()),"capability":hex::encode_lower(self.capability.bytes()),"key":hex::encode_lower(&self.key),"root_seed":self.root_seed.as_ref().map(|s|hex::encode_lower(s))});
        private_file::write_private(
            dir,
            CONFIG,
            &aterm_json::to_vec(&v).map_err(|_| Error::new("configuration encoding failed"))?,
        )?;
        Ok(())
    }
    /// Certify a public installation request. The invitation contains a read key;
    /// deliver it privately and authenticate the root fingerprint out of band.
    pub fn invitation(&self, request: &[u8]) -> Result<Vec<u8>> {
        let actor: Actor = request
            .try_into()
            .map_err(|_| Error::new("invalid installation request length"))?;
        let root = self
            .root_seed
            .as_ref()
            .ok_or_else(|| Error::new("this replica does not hold admission authority"))?;
        let capability = Capability::issue(&self.manifest, root, actor)?;
        let v = json!({"manifest":hex::encode_lower(self.manifest.bytes()),"capability":hex::encode_lower(capability.bytes()),"key":hex::encode_lower(&self.key)});
        aterm_json::to_vec(&v).map_err(|_| Error::new("invitation encoding failed"))
    }
}

struct WriterLock(File);
impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
fn lock(dir: &Path) -> Result<WriterLock> {
    private_file::create_private_dir(dir)?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.join("writer.lock"))
    {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let (file, _) = private_file::open_private(dir, "writer.lock")
        .map_err(|_| Error::new("database writer lock is not private"))?;
    #[expect(
        clippy::disallowed_methods,
        reason = "WriterLock releases by LOCK_UN before closing, including while another child inherited the descriptor"
    )]
    file.try_lock().map_err(|_| {
        Error::new("another local database writer is active; retry after it finishes")
    })?;
    Ok(WriterLock(file))
}
fn missing(dir: &Path, name: &str) -> Result<bool> {
    match private_file::read_private(dir, name, 4096) {
        Err(PrivateFileError::Missing) => Ok(true),
        Ok(_) => Ok(false),
        Err(_) => Err(Error::new(
            "existing database state is not a private regular file",
        )),
    }
}
fn identity(dir: &Path) -> Result<([u8; 32], Actor)> {
    let bytes = private_file::read_private(dir, IDENTITY, 48)
        .map_err(|_| Error::new("local database identity missing or not private"))?;
    if bytes.len() != 48 {
        return Err(Error::new("invalid local database identity"));
    }
    let seed = bytes[..32]
        .try_into()
        .map_err(|_| Error::new("invalid signing seed"))?;
    let mut actor = [0; 48];
    actor[..32].copy_from_slice(&crypto::public(&seed)?);
    actor[32..].copy_from_slice(&bytes[32..]);
    Ok((seed, actor))
}
fn ensure_identity(dir: &Path) -> Result<([u8; 32], Actor)> {
    if missing(dir, IDENTITY)? {
        private_file::write_private(dir, IDENTITY, &crypto::random::<48>()?)?;
    }
    identity(dir)
}
/// Create a private installation identity and return its PUBLIC admission request.
pub fn request(dir: &Path) -> Result<Actor> {
    let _lock = lock(dir)?;
    Ok(ensure_identity(dir)?.1)
}
/// Bootstrap a complete single-machine database. Existing state is never replaced.
pub fn init(dir: &Path, namespace: &str) -> Result<()> {
    let _lock = lock(dir)?;
    if !missing(dir, CONFIG)? {
        return Err(Error::new("database already initialized"));
    }
    let (seed, actor) = ensure_identity(dir)?;
    let root_seed = crypto::random()?;
    let manifest = Manifest::create(&root_seed, namespace)?;
    let capability = Capability::issue(&manifest, &root_seed, actor)?;
    Credentials {
        manifest,
        capability,
        seed,
        key: crypto::random()?,
        root_seed: Some(root_seed),
    }
    .save(dir)
}
/// Join using an invitation for this machine and an independently trusted root key.
pub fn join(dir: &Path, invitation: &[u8], trusted_root: [u8; 32]) -> Result<()> {
    if invitation.len() > 4096 {
        return Err(Error::new("invitation too large"));
    }
    let _lock = lock(dir)?;
    if !missing(dir, CONFIG)? {
        return Err(Error::new("database already initialized"));
    }
    let v: Value =
        aterm_json::from_slice(invitation).map_err(|_| Error::new("invalid invitation"))?;
    let manifest = Manifest::decode(&unhex(field(&v, "manifest")?, 256)?)?;
    if manifest.root != trusted_root {
        return Err(Error::new(
            "invitation root differs from the explicitly trusted key",
        ));
    }
    let capability = Capability::decode(&manifest, &unhex(field(&v, "capability")?, 256)?)?;
    let (seed, actor) = identity(dir)?;
    if actor != capability.actor {
        return Err(Error::new("invitation is for another installation"));
    }
    Credentials {
        manifest,
        capability,
        seed,
        key: decode_hex(field(&v, "key")?)?,
        root_seed: None,
    }
    .save(dir)
}
fn unhex(s: &str, max: usize) -> Result<Vec<u8>> {
    if s.len() > max * 2 || !s.len().is_multiple_of(2) {
        return Err(Error::new("invalid encoded database bytes"));
    }
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = hex::nibble(pair[0]).ok_or_else(|| Error::new("invalid hex"))?;
            let low = hex::nibble(pair[1]).ok_or_else(|| Error::new("invalid hex"))?;
            Ok(high * 16 + low)
        })
        .collect()
}

/// One exclusive local writer. Open for an edit/exchange, not while waiting for peers.
pub struct Database {
    /// Local private key material and peer admission.
    pub credentials: Credentials,
    /// Authenticated read history.
    pub history: History,
    log: BrokerLog,
    append_failed: bool,
    receipts: BTreeMap<Digest, (u64, u64)>,
    _lock: WriterLock,
    _dir: PathBuf,
}
impl Database {
    /// Open the strict journal and replay every bounded authenticated record.
    pub fn open(dir: &Path) -> Result<Self> {
        let lock = lock(dir)?;
        let credentials = Credentials::load(dir)?;
        let log = BrokerLog::open_with(dir.join("operations.astream"), Durability::Strict)?;
        if log.stats().retained_bytes > MAX_JOURNAL {
            return Err(Error::new("database journal exceeds generation capacity"));
        }
        let mut history = History::new(credentials.manifest.root);
        let mut receipts = BTreeMap::new();
        let mut offset = Offset::ZERO;
        loop {
            let batch = log.read_range(offset, 256);
            if batch.is_empty() {
                break;
            }
            for record in &batch {
                if record.subject != "db/operation/2" || record.body.len() < 8 {
                    return Err(Error::new("foreign record in database journal"));
                }
                let received_at = u64::from_le_bytes(
                    record.body[..8]
                        .try_into()
                        .map_err(|_| Error::new("invalid journal receipt"))?,
                );
                let opened =
                    crypto::open(&credentials.manifest, &credentials.key, &record.body[8..])?;
                receipts
                    .entry(opened.header.id)
                    .or_insert((received_at, record.seq.0));
                history.insert(opened)?;
                offset = Offset(
                    record
                        .seq
                        .0
                        .checked_add(1)
                        .ok_or_else(|| Error::new("journal offset exhausted"))?,
                );
            }
        }
        Ok(Self {
            credentials,
            history,
            log,
            append_failed: false,
            receipts,
            _lock: lock,
            _dir: dir.to_path_buf(),
        })
    }
    /// Pure local snapshot; consumers must surface pending history and conflicts.
    pub fn view(&self) -> View {
        self.history.view()
    }
    /// Signed operation metadata together with this replica's first durable receipt.
    /// The received time and append position are local observations, not global order.
    pub fn provenance(&self, id: &Digest) -> Option<Value> {
        let mut value = self.history.provenance(id)?;
        if let Some((time, offset)) = self.receipts.get(id)
            && let Some(object) = value.as_object_mut()
        {
            object.insert("received_at_unix_ms".into(), Value::from(*time));
            object.insert("local_journal_offset".into(), Value::from(*offset));
        }
        Some(value)
    }
    /// Authenticate and durably retain a peer operation. A missing dependency waits;
    /// a duplicate is inert. Acknowledgement follows astream's strict append.
    pub fn ingest(&mut self, wire: &[u8]) -> Result<bool> {
        if self.append_failed {
            return Err(Error::new(
                "an earlier append had an uncertain outcome; reopen the database before any more writes",
            ));
        }
        let opened = crypto::open(&self.credentials.manifest, &self.credentials.key, wire)?;
        if self.history.wire(&opened.header.id).is_some() {
            return Ok(false);
        }
        let mut proposed = self.history.clone();
        proposed.insert(opened)?;
        if self
            .log
            .stats()
            .retained_bytes
            .saturating_add(wire.len() + 256)
            > MAX_JOURNAL
        {
            return Err(Error::new(
                "database journal capacity exhausted; preserve history and start a new generation",
            ));
        }
        let seq = self
            .log
            .stats()
            .head
            .0
            .checked_add(1)
            .ok_or_else(|| Error::new("journal sequence exhausted"))?;
        let received_at = aterm_time::unix_millis()
            .ok_or_else(|| Error::new("system clock cannot represent a receipt timestamp"))?;
        let mut body = received_at.to_le_bytes().to_vec();
        body.extend_from_slice(wire);
        let appended = match self
            .log
            .publish_checked(1, seq, "db/operation/2".into(), body)
        {
            Ok((offset, _)) => offset.0,
            Err(_) => {
                // The bytes may have reached disk before a flush failed. Continuing
                // from the stale in-memory sequence could manufacture a local fork.
                self.append_failed = true;
                return Err(Error::new(
                    "strict database append had an uncertain outcome; reopen and retry the same request identity",
                ));
            }
        };
        self.history = proposed;
        self.receipts
            .insert(aterm_digest::Sha256::digest(wire), (received_at, appended));
        Ok(true)
    }
    /// Author a durable transaction, retrying its previously stored request identity.
    pub fn commit(&mut self, command: Command) -> Result<Digest> {
        if self.append_failed {
            return Err(Error::new(
                "an earlier append had an uncertain outcome; reopen before retrying the same request identity",
            ));
        }
        let control = command.is_control();
        let control_capability = if control {
            Some(Capability::control(
                &self.credentials.manifest,
                self.credentials.root_seed.as_ref().ok_or_else(|| {
                    Error::new("this replica does not hold administrative signing authority")
                })?,
            )?)
        } else {
            None
        };
        let capability = control_capability
            .as_ref()
            .unwrap_or(&self.credentials.capability);
        let actor = capability.actor;
        if let Some(id) = self.history.retry(&actor, &command)? {
            return Ok(id);
        }
        let view = self.view();
        if command.changes().is_some() && !control && view.revocations.contains_key(&actor) {
            return Err(Error::new(
                "this installation's catalog writes have been retired; bounded grant receipts remain valid",
            ));
        }
        if let Some(changes) = command.changes() {
            let view = self.view();
            for change in changes {
                if view
                    .cells
                    .get(&(change.collection.clone(), change.key.clone()))
                    .is_some_and(|cell| cell.stopped)
                {
                    return Err(Error::new(
                        "object generation stopped; use a new object key",
                    ));
                }
                if change.collection == "machines"
                    && let Some(value) = &change.value
                {
                    for name in crate::machines::names(value) {
                        if view
                            .machine_names
                            .get(&name)
                            .is_some_and(|owners| owners.iter().any(|owner| owner != &change.key))
                        {
                            return Err(Error::new(
                                "machine name is already in another machine's history; keep the proposal for review",
                            ));
                        }
                    }
                }
            }
        }
        let projection = if control || matches!(&command, Command::Spend { .. }) {
            self.history.index.project()
        } else {
            view.causal
        };
        if projection
            .quarantined
            .iter()
            .any(|a| a == &actor || a[..32] == self.credentials.manifest.root)
        {
            return Err(Error::new(
                "local writer or admission authority equivocated; recovery required",
            ));
        }
        let previous = projection
            .ordered
            .iter()
            .filter_map(|id| self.history.index.get(id))
            .filter(|h| h.actor == actor)
            .max_by_key(|h| h.sequence);
        let sequence = previous
            .map_or(Some(1), |h| h.sequence.checked_add(1))
            .ok_or_else(|| Error::new("actor sequence exhausted"))?;
        if self
            .history
            .index
            .inventory()
            .filter_map(|id| self.history.index.get(id))
            .any(|h| h.actor == actor && h.sequence >= sequence)
        {
            return Err(Error::new(
                "local actor has unprojected history; repair before writing",
            ));
        }
        let previous = previous.map(|h| h.id);
        let mut parents = projection.heads;
        if control {
            parents.clear();
        }
        // A peer may have observed our last write, leaving only its descendant
        // as a maximal head. The dense writer chain still names its exact predecessor.
        if let Some(previous) = previous {
            parents.insert(previous);
        }
        let parents: Vec<_> = parents.into_iter().collect();
        let plaintext = command.encode()?;
        if Command::decode(&plaintext)? != command {
            return Err(Error::new(
                "database command encoding would change its meaning",
            ));
        }
        if let Command::Revoke { actor: revoked, .. } = &command
            && *revoked == actor
        {
            return Err(Error::new("authority stream cannot revoke itself"));
        }
        let wire = crypto::seal(
            &self.credentials.manifest,
            &self.credentials.key,
            if control {
                self.credentials
                    .root_seed
                    .as_ref()
                    .ok_or_else(|| Error::new("administrative key unavailable"))?
            } else {
                &self.credentials.seed
            },
            capability,
            crypto::Causal {
                sequence,
                previous,
                parents: &parents,
            },
            &plaintext,
        )?;
        let id = aterm_digest::Sha256::digest(&wire);
        self.ingest(&wire)?;
        Ok(id)
    }
    /// Append a compensation only while every affected key still has the exact
    /// target as its sole current candidate. Concurrent or later edits require
    /// explicit resolution. Permanent stops and external/resource actions refuse.
    pub fn revert(&mut self, request: [u8; 16], operation: Digest) -> Result<Digest> {
        let target = self
            .history
            .command(&operation)
            .map(|(_, c)| c.clone())
            .ok_or_else(|| Error::new("target is not a supported retained command"))?;
        let actor = if target.is_control() {
            self.credentials.manifest.control_actor()
        } else {
            self.credentials.capability.actor
        };
        if let Some((_, old)) = self.history.find_request(&actor, request) {
            if matches!(old,Command::Revert{operation:prior,..}if *prior==operation) {
                return self
                    .history
                    .retry(&actor, old)?
                    .ok_or_else(|| Error::new("revert receipt is absent"));
            }
            return Err(Error::new(
                "request identity already belongs to another command",
            ));
        }
        let affected=target.changes().ok_or_else(||Error::new("resource consumption, allocation and revocation require domain reconciliation; they cannot be reverted"))?;
        if matches!(target, Command::Write { .. }) && affected.iter().any(|c| c.value.is_none()) {
            return Err(Error::new(
                "permanent safety stops cannot be reverted; authorize a new object generation",
            ));
        }
        let view = self.view();
        if !view.causal.ordered.contains(&operation) || view.invalid.contains(&operation) {
            return Err(Error::new("target is pending, refused or quarantined"));
        }
        for change in affected {
            let cell = view
                .cells
                .get(&(change.collection.clone(), change.key.clone()))
                .ok_or_else(|| Error::new("target is no longer current"))?;
            if cell.stopped
                || cell.candidates.len() != 1
                || cell.candidates[0].operation != operation
            {
                return Err(Error::new(
                    "another edit changed an affected key; resolve it before reverting",
                ));
            }
        }
        let past = self.history.before(&operation)?;
        let mut changes = Vec::new();
        for change in affected {
            let previous = past
                .cells
                .get(&(change.collection.clone(), change.key.clone()));
            if previous.is_some_and(|c| c.stopped || c.candidates.len() > 1) {
                return Err(Error::new(
                    "target's previous value is stopped or conflicted; explicit resolution is required",
                ));
            }
            changes.push(Change {
                collection: change.collection.clone(),
                key: change.key.clone(),
                value: previous
                    .and_then(|c| c.candidates.first())
                    .map(|c| c.value.clone())
                    .filter(|v| !v.is_null()),
            });
        }
        self.commit(Command::Revert {
            request,
            operation,
            changes,
        })
    }
    /// Mint an immutable fixed budget. Its name includes the exact resource/window;
    /// callers must never create overlapping names for the same finite capacity.
    pub fn allocate(
        &mut self,
        request: [u8; 16],
        name: String,
        total: u64,
        shares: BTreeMap<Actor, u64>,
    ) -> Result<Digest> {
        let command = Command::Budget {
            request,
            name: name.clone(),
            total,
            shares,
        };
        if let Some(id) = self
            .history
            .retry(&self.credentials.manifest.control_actor(), &command)?
        {
            return Ok(id);
        }
        if self.history.has_budget(&name) {
            return Err(Error::new(
                "budget generation already exists, including pending or quarantined history; capacity cannot be reminted",
            ));
        }
        self.commit(command)
    }
    /// Reserve admission units before external work. A crash after this receipt may
    /// waste units; it cannot safely refund an unobserved vendor side effect.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbOfflineRights",
            action = "Separate",
            reason = "A network partition is an environment action; this local debit has no connectivity input. The rights_state conformance test exercises both environment states."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbOfflineRights",
            action = "Reconnect",
            reason = "Reconnection is an environment action; this local debit cannot observe or reclaim a remote share. The rights_state conformance test exercises both environment states."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbOfflineRights",
            action = "SpendA",
            project = "aterm_db::tests::rights_state"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbOfflineRights",
            action = "SpendB",
            project = "aterm_db::tests::rights_state"
        )
    )]
    pub fn spend(&mut self, request: [u8; 16], name: &str, cost: u64) -> Result<Digest> {
        let view = self.view();
        if view.authority_fault
            || view
                .causal
                .quarantined
                .contains(&self.credentials.capability.actor)
        {
            return Err(Error::new(
                "authority or local writer equivocation locks admission until recovery",
            ));
        }
        let grants = view
            .budgets
            .get(name)
            .filter(|g| g.len() == 1)
            .ok_or_else(|| Error::new("budget is missing or has conflicting allocations"))?;
        let grant = grants[0];
        let actor = self.credentials.capability.actor;
        if view.disputed_shares.contains(&(grant, actor)) {
            return Err(Error::new(
                "disputed expenditure evidence locks this machine's share until reconciliation",
            ));
        }
        if view.invalid.iter().any(|id|matches!(self.history.command(id),Some((author,Command::Spend{grant:bad,..}))if *author==actor&&bad==&grant)){return Err(Error::new("invalid accounting evidence locks this machine's share until reconciliation"));}
        if let Some((id, old)) = self.history.find_request(&actor, request) {
            self.history.retry(&actor, old)?;
            return match old {
                Command::Spend {
                    grant: old_grant,
                    cost: old_cost,
                    ..
                } if *old_grant == grant && *old_cost == cost => Ok(id),
                _ => Err(Error::new("request identity reused for another debit")),
            };
        }
        let limit = match self.history.command(&grant) {
            Some((_, Command::Budget { shares, .. })) => shares.get(&actor).copied().unwrap_or(0),
            _ => 0,
        };
        let spent = view.spent.get(&(grant, actor)).copied().unwrap_or(0);
        let cumulative = spent
            .checked_add(cost)
            .ok_or_else(|| Error::new("budget debit exceeds numeric capacity"))?;
        if !aterm_db_core::admissible(limit, spent, cumulative) {
            return Err(Error::new(
                "insufficient rights assigned to this machine; an absent peer's share remains reserved",
            ));
        }
        self.commit(Command::Spend {
            request,
            grant,
            cost,
            cumulative,
        })
    }
    /// Retire a writer's catalog edits by exact hash ancestry. This does not reclaim
    /// offline budget shares or revoke credentials at the service provider.
    pub fn revoke_writes(
        &mut self,
        request: [u8; 16],
        actor: Actor,
        through: Option<Digest>,
    ) -> Result<Digest> {
        if let Some(tip) = through
            && !self
                .history
                .index
                .get(&tip)
                .is_some_and(|h| h.actor == actor)
        {
            return Err(Error::new(
                "revocation cutoff must name a retained record of that exact writer",
            ));
        }
        self.commit(Command::Revoke {
            request,
            actor,
            through,
        })
    }
}
