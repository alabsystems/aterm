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
/// Most retained journal bytes in one database generation.
pub(crate) const MAX_JOURNAL: usize = 128 * 1024 * 1024;

/// Dataset keys and a local installation identity. Never formatted or printed.
pub struct Credentials {
    /// Verified immutable dataset descriptor.
    pub manifest: Manifest,
    /// Root-signed admission for this local identity.
    pub capability: Capability,
    pub(crate) seed: [u8; 32],
    pub(crate) key: [u8; 32],
    root_seed: Option<[u8; 32]>,
    /// The fleet tables' dataset, whose root every holder of `fleet.key` can derive: it has no
    /// authority stream, so a control record stays inert evidence and counts for nothing.
    no_authority: bool,
}
impl Credentials {
    /// Load private bounded state and prove its signing identity matches its admission.
    pub fn load(dir: &Path) -> Result<Self> {
        let judged = private_file::JudgedDir::judge(dir)
            .map_err(|_| Error::new("database configuration missing or not private"))?;
        let bytes = zeroize::Zeroizing::new(
            judged
                .read(CONFIG, 4096)
                .map_err(|_| Error::new("database configuration missing or not private"))?,
        );
        Self::from_config_bytes(&judged, &bytes)
    }
    fn from_config_bytes(judged: &private_file::JudgedDir, bytes: &[u8]) -> Result<Self> {
        let v: Value = aterm_json::from_slice(bytes)
            .map_err(|_| Error::new("invalid database configuration"))?;
        let manifest = Manifest::decode(&unhex(field(&v, "manifest")?, 256)?)?;
        let capability = Capability::decode(&manifest, &unhex(field(&v, "capability")?, 256)?)?;
        if capability.control {
            return Err(Error::new(
                "installation capability cannot be an administrative control certificate",
            ));
        }
        let (seed, actor) = identity_in(judged)?;
        if actor != capability.actor {
            return Err(Error::new(
                "database admission belongs to another installation",
            ));
        }
        let key = decode_hex(field(&v, "key")?)?;
        let root_seed = match v.get("root_seed") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(decode_hex(s)?),
            Some(_) => return Err(Error::new("invalid database configuration")),
        };
        let no_authority = match v.get("no_authority") {
            None => false,
            Some(Value::Bool(set)) => *set,
            Some(_) => return Err(Error::new("invalid database configuration")),
        };
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
            no_authority,
        })
    }
    fn save(&self, dir: &Path) -> Result<()> {
        let key_hex = zeroize::Zeroizing::new(hex::encode_lower(&self.key));
        let root_hex = self
            .root_seed
            .as_ref()
            .map(|s| zeroize::Zeroizing::new(hex::encode_lower(s)));
        let v = json!({"manifest":hex::encode_lower(self.manifest.bytes()),"capability":hex::encode_lower(self.capability.bytes()),"key":key_hex.as_str(),"root_seed":root_hex.as_deref(),"no_authority":self.no_authority});
        let encoded = zeroize::Zeroizing::new(
            aterm_json::to_vec(&v).map_err(|_| Error::new("configuration encoding failed"))?,
        );
        private_file::write_private(dir, CONFIG, &encoded)?;
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
    pub(crate) fn has_control_authority(&self) -> bool {
        self.root_seed.is_some()
    }
}

#[cfg(test)]
mod frontier_tests;
impl Drop for Credentials {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.seed.zeroize();
        self.key.zeroize();
        if let Some(root_seed) = &mut self.root_seed {
            root_seed.zeroize();
        }
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
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(dir.join("writer.lock"))
    {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let (file, _) = aterm_uds::kept_out(|| private_file::open_private(dir, "writer.lock"))
        .map_err(|_| Error::new("database writer lock is not private"))?;
    #[expect(
        clippy::disallowed_methods,
        reason = "WriterLock releases by LOCK_UN before closing, including while another child inherited the descriptor"
    )]
    let locked = file.try_lock();
    match locked {
        Ok(()) => Ok(WriterLock(file)),
        Err(std::fs::TryLockError::WouldBlock) => Err(Error::busy()),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
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
    let judged = private_file::JudgedDir::judge(dir)
        .map_err(|_| Error::new("local database identity missing or not private"))?;
    identity_in(&judged)
}
fn identity_in(judged: &private_file::JudgedDir) -> Result<([u8; 32], Actor)> {
    let bytes = zeroize::Zeroizing::new(
        judged
            .read(IDENTITY, 48)
            .map_err(|_| Error::new("local database identity missing or not private"))?,
    );
    parse_identity(&bytes)
}
fn parse_identity(bytes: &[u8]) -> Result<([u8; 32], Actor)> {
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
    match private_file::read_private(dir, IDENTITY, 4096) {
        Ok(bytes) => {
            let bytes = zeroize::Zeroizing::new(bytes);
            if bytes.len() > 48 {
                return Err(Error::new("local database identity missing or not private"));
            }
            parse_identity(&bytes)
        }
        Err(PrivateFileError::Missing) => {
            let generated = zeroize::Zeroizing::new(crypto::random::<48>()?);
            private_file::write_private(dir, IDENTITY, &*generated)?;
            identity(dir)
        }
        Err(_) => Err(Error::new(
            "existing database state is not a private regular file",
        )),
    }
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
    let root_seed = zeroize::Zeroizing::new(crypto::random()?);
    let manifest = Manifest::create(&root_seed, namespace)?;
    let capability = Capability::issue(&manifest, &root_seed, actor)?;
    Credentials {
        manifest,
        capability,
        seed,
        key: crypto::random()?,
        root_seed: Some(*root_seed),
        no_authority: false,
    }
    .save(dir)
}
/// What the fleet secret decides about the fleet tables' dataset.
struct FleetKeys {
    root_seed: [u8; 32],
    manifest: Manifest,
    key: [u8; 32],
}
impl FleetKeys {
    fn derive(fleet_secret: &[u8]) -> Result<Self> {
        use zeroize::Zeroizing;
        let prk = Zeroizing::new(aterm_digest::hkdf_extract(
            b"aterm-db/fleet-tables/1",
            fleet_secret,
        ));
        let root_seed = Zeroizing::new(aterm_digest::hkdf_expand_32(&prk, b"root-seed"));
        let id = aterm_digest::hkdf_expand_32(&prk, b"dataset-id");
        let incarnation = aterm_digest::hkdf_expand_32(&prk, b"control-incarnation");
        let mut control_incarnation = [0u8; 16];
        control_incarnation.copy_from_slice(&incarnation[..16]);
        Ok(Self {
            manifest: Manifest::deterministic(&root_seed, id, control_incarnation, "alab")?,
            root_seed: *root_seed,
            key: aterm_digest::hkdf_expand_32(&prk, b"encryption-key"),
        })
    }
    fn hold(&self, credentials: &Credentials) -> bool {
        credentials.manifest.bytes() == self.manifest.bytes() && credentials.key == self.key
    }
}
impl Drop for FleetKeys {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.root_seed.zeroize();
        self.key.zeroize();
    }
}
/// Ensure the deterministic fleet-shared database is initialized at `dir`.
/// Every machine in the fleet derives the same dataset descriptor and encryption key from
/// `fleet_secret`, while signing causal operations with its own local `identity`. The root seed
/// is never kept, and because every member can derive it anyway, the dataset takes no authority
/// stream at all: a control record has no effect on a replica (`no_authority`).
pub fn ensure_fleet(dir: &Path, fleet_secret: &[u8]) -> Result<()> {
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = private_file::create_private_dir(parent);
    }
    let keys = FleetKeys::derive(fleet_secret)?;
    let _lock = lock(dir)?;
    let judged = private_file::JudgedDir::judge(dir)
        .map_err(|_| Error::new("existing database state is not a private regular file"))?;
    if let Some(config_bytes) = match judged.read(CONFIG, 4096) {
        Ok(bytes) => Some(zeroize::Zeroizing::new(bytes)),
        Err(PrivateFileError::Missing) => None,
        Err(_) => {
            return Err(Error::new(
                "existing database state is not a private regular file",
            ));
        }
    } {
        let mut existing = Credentials::from_config_bytes(&judged, &config_bytes)?;
        if !keys.hold(&existing) {
            return Err(Error::new(
                "this database belongs to another fleet key; `aterm db rekey` moves its rows onto the current one",
            ));
        }
        if existing.root_seed.is_some() || !existing.no_authority {
            existing.root_seed = None;
            existing.no_authority = true;
            existing.save(dir)?;
        }
        return Ok(());
    }
    let (seed, actor) = ensure_identity(dir)?;
    Credentials {
        capability: Capability::issue(&keys.manifest, &keys.root_seed, actor)?,
        manifest: keys.manifest.clone(),
        seed,
        key: keys.key,
        root_seed: None,
        no_authority: true,
    }
    .save(dir)
}
/// Whether the database at `dir` already belongs to the dataset `fleet_secret` decides.
pub(crate) fn belongs_to_fleet(dir: &Path, fleet_secret: &[u8]) -> Result<bool> {
    let keys = FleetKeys::derive(fleet_secret)?;
    let _lock = lock(dir)?;
    Ok(keys.hold(&Credentials::load(dir)?))
}
/// Whether `dir` holds a dataset other than the one `fleet_secret` decides.
pub(crate) fn holds_another_dataset(dir: &Path, fleet_secret: &[u8]) -> Result<bool> {
    if missing(dir, CONFIG)? {
        return Ok(false);
    }
    belongs_to_fleet(dir, fleet_secret).map(|belongs| !belongs)
}
/// A replica of the fleet tables as a member that used its fleet key to sign a control record:
/// keeps the root seed and a history that admits the record, as no shipped build can.
#[cfg(test)]
pub(crate) fn test_forge_authority(dir: &Path, fleet_secret: &[u8]) -> Result<()> {
    let keys = FleetKeys::derive(fleet_secret)?;
    ensure_fleet(dir, fleet_secret)?;
    let mut credentials = Credentials::load(dir)?;
    credentials.root_seed = Some(keys.root_seed);
    credentials.no_authority = false;
    credentials.save(dir)
}
/// Undo the `no_authority` mark, as a replica a build before it left behind.
#[cfg(test)]
pub(crate) fn test_forget_no_authority(dir: &Path) -> Result<()> {
    let mut credentials = Credentials::load(dir)?;
    credentials.no_authority = false;
    credentials.save(dir)
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
    if capability.control {
        return Err(Error::new(
            "installation capability cannot be an administrative control certificate",
        ));
    }
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
        no_authority: false,
    }
    .save(dir)
}
fn unhex(s: &str, max: usize) -> Result<Vec<u8>> {
    if s.len() > max * 2 || !s.len().is_multiple_of(2) {
        return Err(Error::new("invalid encoded database bytes"));
    }
    let lower_nibble = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in s.as_bytes().as_chunks::<2>().0 {
        let high =
            lower_nibble(pair[0]).ok_or_else(|| Error::new("invalid encoded database bytes"))?;
        let low =
            lower_nibble(pair[1]).ok_or_else(|| Error::new("invalid encoded database bytes"))?;
        out.push(high * 16 + low);
    }
    Ok(out)
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
        Self::open_as(dir, false)
    }
    /// Open a fleet dataset to move its rows, ignoring a control record as every current replica
    /// does: [`ensure_fleet`] only marks a database of the current key, so one that a rotation
    /// left behind may never have been marked.
    pub(crate) fn open_for_rekey(dir: &Path) -> Result<Self> {
        Self::open_as(dir, true)
    }
    fn open_as(dir: &Path, no_authority: bool) -> Result<Self> {
        let lock = lock(dir)?;
        let mut credentials = Credentials::load(dir)?;
        credentials.no_authority |= no_authority;
        let log = BrokerLog::open_with(dir.join("operations.astream"), Durability::Strict)?;
        if log.stats().retained_bytes > MAX_JOURNAL {
            return Err(Error::new("database journal exceeds generation capacity"));
        }
        let mut history = if credentials.no_authority {
            History::without_authority(credentials.manifest.root)
        } else {
            History::new(credentials.manifest.root)
        };
        let mut receipts = BTreeMap::new();
        let mut offset = Offset::ZERO;
        loop {
            let batch = log.read_range(offset, 256);
            if batch.is_empty() {
                break;
            }
            for record in &batch {
                if record.seq != offset
                    || record.subject != "db/operation/2"
                    || record.body.len() < 8
                {
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
                history.replay(opened)?;
                offset = Offset(
                    record
                        .seq
                        .0
                        .checked_add(1)
                        .ok_or_else(|| Error::new("journal offset exhausted"))?,
                );
            }
        }
        if log.stats().head != offset {
            return Err(Error::new("foreign record in database journal"));
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
    /// Retained bytes of the journal, which [`MAX_JOURNAL`] bounds.
    pub(crate) fn journal_bytes(&self) -> usize {
        self.log.stats().retained_bytes
    }
    /// Signed operation metadata together with this replica's first durable receipt.
    /// The received time and append position are local observations, not global order.
    pub fn provenance(&self, id: &Digest) -> Option<Value> {
        let mut value = self.history.provenance(id)?;
        self.attach_receipt(id, &mut value);
        Some(value)
    }

    pub(crate) fn all_provenance(&self) -> Vec<Value> {
        let projection = self.history.project(&std::collections::BTreeSet::new());
        let ordered: std::collections::BTreeSet<Digest> =
            projection.ordered.iter().copied().collect();
        self.history
            .index
            .inventory()
            .filter_map(|id| {
                let mut value =
                    self.history
                        .provenance_with(id, &projection.quarantined, |id| ordered.contains(id))?;
                self.attach_receipt(id, &mut value);
                Some(value)
            })
            .collect()
    }

    fn attach_receipt(&self, id: &Digest, value: &mut Value) {
        if let Some((time, offset)) = self.receipts.get(id)
            && let Some(object) = value.as_object_mut()
        {
            object.insert("received_at_unix_ms".into(), Value::from(*time));
            object.insert("local_journal_offset".into(), Value::from(*offset));
        }
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
        let mut body = Vec::with_capacity(8 + wire.len());
        body.extend_from_slice(&received_at.to_le_bytes());
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
    /// Catalog writes join wide frontiers with value-free records. Fixed-share
    /// receipts depend on their grant and this writer's predecessor.
    pub fn commit(&mut self, command: Command) -> Result<Digest> {
        if self.append_failed {
            return Err(Error::new(
                "an earlier append had an uncertain outcome; reopen before retrying the same request identity",
            ));
        }
        if self.credentials.no_authority && command.stops() {
            return Err(Error::new(
                "this dataset has no authority stream, so no member can stop a key for good; delete the row instead",
            ));
        }
        loop {
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
            let view = self.view();
            if let Some(id) = self.history.retry(&actor, &command)? {
                // A valid debit remains liability evidence when one of its catalog
                // ancestors is withdrawn. Its retry must use that same projection.
                let eligible = if matches!(&command, Command::Spend { .. }) && !view.authority_fault
                {
                    self.history.index.project().ordered.contains(&id)
                } else {
                    view.causal.ordered.contains(&id)
                };
                if !eligible || view.invalid.contains(&id) {
                    return Err(Error::new(
                        "the prior operation receipt is not currently eligible",
                    ));
                }
                return Ok(id);
            }
            if command.changes().is_some() && !control && view.revocations.contains_key(&actor) {
                return Err(Error::new(
                    "this installation's catalog writes have been retired; bounded grant receipts remain valid",
                ));
            }
            if let Some(changes) = command.changes() {
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
                            if view.machine_names.get(&name).is_some_and(|owners| {
                                owners.iter().any(|owner| owner != &change.key)
                            }) {
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
            if projection.quarantined.iter().any(|a| {
                a == &actor
                    || (!self.credentials.no_authority && a[..32] == self.credentials.manifest.root)
            }) {
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
            } else if let Command::Spend { grant, .. } = &command {
                // Fixed allocations need no other writer's current frontier. Keep
                // only the authenticated grant and the dense local receipt chain.
                parents.clear();
                if projection.ordered.contains(grant) {
                    parents.insert(*grant);
                }
            }
            let joining = !control
                && !matches!(&command, Command::Spend { .. })
                && parents.len() + usize::from(previous.is_some_and(|id| !parents.contains(&id)))
                    > astream_wire::sync::MAX_PARENTS;
            if joining {
                parents = parents
                    .into_iter()
                    .take(astream_wire::sync::MAX_PARENTS - 1)
                    .collect();
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
            let plaintext = if joining {
                Command::Merge {
                    request: crypto::random()?,
                }
                .encode()?
            } else {
                plaintext
            };
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
            if !joining {
                return Ok(id);
            }
        }
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
        if target.is_control() && self.credentials.root_seed.is_none() {
            return Err(Error::new(
                "this replica does not hold administrative signing authority",
            ));
        }
        let actor = if target.is_control() {
            self.credentials.manifest.control_actor()
        } else {
            self.credentials.capability.actor
        };
        if let Some((_, old)) = self
            .history
            .find_request(&actor, request)
            .map(|(id, c)| (id, c.clone()))
        {
            if matches!(&old, Command::Revert { operation: prior, .. } if *prior == operation) {
                return self.commit(old);
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
        let mut changes = Vec::with_capacity(affected.len());
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
        if self.credentials.root_seed.is_none() {
            return Err(Error::new(
                "this replica does not hold administrative signing authority",
            ));
        }
        let command = Command::Budget {
            request,
            name: name.clone(),
            total,
            shares,
        };
        if self
            .history
            .retry(&self.credentials.manifest.control_actor(), &command)?
            .is_some()
        {
            return self.commit(command);
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
                "incomplete or disputed accounting history locks this machine's share until reconciliation",
            ));
        }
        if view.invalid.iter().any(|id|matches!(self.history.command(id),Some((author,Command::Spend{grant:bad,..}))if *author==actor&&bad==&grant)){return Err(Error::new("invalid accounting evidence locks this machine's share until reconciliation"));}
        if let Some((_, old)) = self
            .history
            .find_request(&actor, request)
            .map(|(id, c)| (id, c.clone()))
        {
            return match &old {
                Command::Spend {
                    grant: old_grant,
                    cost: old_cost,
                    ..
                } if *old_grant == grant && *old_cost == cost => self.commit(old),
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

#[cfg(test)]
mod replay_tests {
    use super::*;

    const SECRET: &[u8] = b"1111111111111111111111111111111111111111";

    /// A build before the `no_authority` mark counted a control record against no quota, and the
    /// journal it left has to open still, or its rows are out of reach, its rekey included.
    /// NEGATIVE CONTROL: with replay going through `History::insert`, the open fails:
    /// `` writer's retained history quota is exhausted ``.
    #[test]
    fn control_records_an_earlier_build_kept_past_a_quota_do_not_close_the_folder() {
        let root = crate::tests::tmp();
        let victim = root.path().join("victim");
        ensure_fleet(&victim, SECRET).unwrap();
        test_forget_no_authority(&victim).unwrap();
        let keys = FleetKeys::derive(SECRET).unwrap();
        let cap = Capability::control(&keys.manifest, &keys.root_seed).unwrap();
        let plain = format!(
            "{{\"v\":1,\"pad\":\"{}\"}}",
            "x".repeat(crypto::MAX_BODY - 32)
        );
        let mut unmarked = Database::open(&victim).unwrap();
        let mut previous: Option<Digest> = None;
        for n in 0..(crate::database::MAX_WRITER_BYTES / crypto::MAX_BODY + 2) as u64 {
            let parents: Vec<Digest> = previous.into_iter().collect();
            let wire = crypto::seal(
                &keys.manifest,
                &keys.key,
                &keys.root_seed,
                &cap,
                crypto::Causal {
                    sequence: n + 1,
                    previous,
                    parents: &parents,
                },
                plain.as_bytes(),
            )
            .unwrap();
            previous = Some(aterm_digest::Sha256::digest(&wire));
            unmarked.ingest(&wire).unwrap();
        }
        drop(unmarked);

        ensure_fleet(&victim, SECRET).unwrap();
        assert!(Database::open(&victim).is_ok());
    }
}
