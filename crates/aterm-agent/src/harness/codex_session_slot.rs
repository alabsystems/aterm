// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-process credential materializations. A slot never creates a grant or an
//! assignment: both must arrive as current, typed native owner evidence.

use super::{accounts, cli::Env, codex_assignment, codex_grant_owner as owner};
use aterm_secrets::{SecretBytes, age, client};
use aterm_uds::private_file;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroize::Zeroize;

const MAX_DOCUMENT: usize = 16 * 1024;
const MAX_RECORD: usize = 64 * 1024;
const MARKER: &str = "auth.json.aterm-slot";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Caller {
    pub sid: String,
    pub pid: u32,
    pub birth: u64,
}

impl Caller {
    fn valid(&self) -> bool {
        self.pid > 1
            && self.birth > 0
            && self.sid.starts_with("s-")
            && self.sid.len() > 2
            && self.sid.len() <= 64
            && self
                .sid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub generation: u64,
    pub digest: String,
}

impl Revision {
    fn valid(&self) -> bool {
        self.generation > 0 && hex(&self.digest, 64)
    }
}

pub struct RefreshRequest {
    pub slot: String,
    pub operation: String,
    pub expected_digest: String,
    authority: Option<RefreshAuthority>,
}

impl RefreshRequest {
    pub fn new(slot: String, operation: String, expected_digest: String) -> Self {
        Self {
            slot,
            operation,
            expected_digest,
            authority: None,
        }
    }

    pub(super) fn authority(&self) -> Option<&RefreshAuthority> {
        self.authority.as_ref()
    }
}

/// Authenticated slot evidence, never supplied by the wire parser. A late
/// request for A must not be interpreted as authority to refresh or install B.
pub(super) struct RefreshAuthority {
    binding: owner::GrantBinding,
    assignment_generation: u64,
    source_authority: codex_assignment::SourceAuthority,
    materialized_revision: Revision,
}

impl RefreshAuthority {
    pub(super) fn binding(&self) -> &owner::GrantBinding {
        &self.binding
    }
    pub(super) fn assignment_generation(&self) -> u64 {
        self.assignment_generation
    }
    pub(super) fn source_authority(&self) -> &codex_assignment::SourceAuthority {
        &self.source_authority
    }
    pub(super) fn materialized_revision(&self) -> &Revision {
        &self.materialized_revision
    }
}

pub enum SourceProgress {
    Waiting,
    Ready(SourceHandle),
}
pub enum RefreshProgress {
    Waiting(Revision),
    Ready(Revision),
    Retaining(RefreshRetention),
}

#[must_use = "keep the app operation alive until encrypted response settlement"]
pub struct RefreshRetention {
    inner: Box<owner::OwnedRefreshRetention>,
}
impl RefreshRetention {
    pub fn retry(self) -> Result<RefreshContinuation, Self> {
        self.inner
            .retry()
            .map(|inner| RefreshContinuation { inner })
            .map_err(|inner| Self { inner })
    }
}
impl std::fmt::Debug for RefreshRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RefreshRetention(redacted)")
    }
}

#[must_use = "finish shared recovery before checking caller cancellation"]
pub struct RefreshContinuation {
    inner: owner::OwnerRefreshContinuation,
}
impl std::fmt::Debug for RefreshContinuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RefreshContinuation(redacted)")
    }
}
pub enum RefreshRecovery {
    Complete,
    Unavailable,
    Retaining(RefreshRetention),
}
impl RefreshContinuation {
    pub fn reconcile(self, env: &Env, scope: &super::account_work::Scope) -> RefreshRecovery {
        match self.inner.reconcile(env, scope) {
            Ok(()) => RefreshRecovery::Complete,
            Err(owner::OwnerRefreshError::Unavailable) => RefreshRecovery::Unavailable,
            Err(owner::OwnerRefreshError::Retaining(inner)) => {
                RefreshRecovery::Retaining(RefreshRetention { inner })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotError {
    InvalidRequest,
    Denied,
    Unavailable,
}

impl std::fmt::Display for SlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("native credential source unavailable")
    }
}
impl std::error::Error for SlotError {}

pub struct SourceHandle {
    binding: std::sync::Arc<owner::GrantBinding>,
    slot: String,
    path: PathBuf,
    account: String,
    user: String,
    revision: Revision,
    assignment_generation: u64,
    native_intent: Option<String>,
}

impl std::fmt::Debug for SourceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceHandle(redacted)")
    }
}

impl SourceHandle {
    // Descriptive authenticated grant identity for native registration. This
    // does not extend the source receipt into assignment or activation authority.
    pub(super) fn binding(&self) -> &owner::GrantBinding {
        &self.binding
    }

    pub fn slot(&self) -> &str {
        &self.slot
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn account(&self) -> &str {
        &self.account
    }
    pub fn user(&self) -> &str {
        &self.user
    }
    pub fn credential_generation(&self) -> u64 {
        self.revision.generation
    }
    pub fn digest(&self) -> &str {
        &self.revision.digest
    }
    pub fn assignment_generation(&self) -> u64 {
        self.assignment_generation
    }
    pub fn native_intent(&self) -> Option<&str> {
        self.native_intent.as_deref()
    }
}

/// This receipt retains both the materialization flock and metadata claim until
/// the owner has committed its acknowledgement. It has no data constructor.
pub(super) struct MaterializationReceipt {
    caller: Caller,
    slot: String,
    revision: Revision,
    _auth: super::codex_app_login::AuthLock,
    _store: super::pending_native_store::Store,
}
impl MaterializationReceipt {
    pub(super) fn caller(&self) -> &Caller {
        &self.caller
    }
    pub(super) fn slot(&self) -> &str {
        &self.slot
    }
    pub(super) fn generation(&self) -> u64 {
        self.revision.generation
    }
    pub(super) fn digest(&self) -> &str {
        &self.revision.digest
    }
}
impl std::fmt::Debug for MaterializationReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MaterializationReceipt(redacted)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    caller: Caller,
    binding: owner::GrantBinding,
    assignment_generation: u64,
    source_authority: codex_assignment::SourceAuthority,
    revision: Revision,
}

impl Record {
    fn valid(&self) -> bool {
        self.schema == 2
            && self.caller.valid()
            && self.binding.request.valid()
            && self.binding.generation > 0
            && self.source_authority.valid(self.assignment_generation)
            && self.revision.valid()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    record: Record,
    mac: String,
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn actor_key(caller: &Caller) -> Result<String, String> {
    if !caller.valid() {
        return Err("invalid slot caller".into());
    }
    let bytes = aterm_json::to_vec(&("aterm/codex-session/v1", caller))
        .map_err(|_| "slot caller unavailable")?;
    let digest = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&bytes));
    Ok(aterm_codec::hex::encode_lower(
        format!("codex/session/{digest}").as_bytes(),
    ))
}

fn slot_key(caller: &Caller, binding: &owner::GrantBinding) -> Result<String, String> {
    if !caller.valid() || !binding.request.valid() || binding.generation == 0 {
        return Err("invalid slot identity".into());
    }
    let bytes = SecretBytes::new(
        aterm_json::to_vec(&("aterm/codex-session-grant/v1", caller, binding))
            .map_err(|_| "slot identity unavailable")?,
    );
    let digest = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes.as_slice()));
    Ok(aterm_codec::hex::encode_lower(
        format!("codex/session/{digest}").as_bytes(),
    ))
}

fn location(env: &Env, caller: &Caller) -> Result<(PathBuf, PathBuf, String), String> {
    let key = actor_key(caller)?;
    let root = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("slot store unavailable")?
        .dir
        .join("session-slots");
    let home = env.home.as_ref().ok_or("slot home unavailable")?;
    if !root.is_absolute() || !home.is_absolute() {
        return Err("slot location unavailable".into());
    }
    Ok((root.join(&key), home.clone(), key))
}

fn materialization_location(
    env: &Env,
    caller: &Caller,
    binding: &owner::GrantBinding,
) -> Result<PathBuf, String> {
    let (directory, _, _) = location(env, caller)?;
    Ok(directory
        .join("credentials")
        .join(slot_key(caller, binding)?))
}

fn machine(home: &Path) -> Result<age::Identity, String> {
    client::read_machine_identity(&client::Paths::locate_for_home(home))
        .map_err(|_| "slot identity unavailable")?
        .ok_or("slot identity unavailable".into())
}

fn mac(identity: &age::Identity, record: &Record) -> Result<String, String> {
    let bytes = SecretBytes::new(
        aterm_json::to_vec(&("aterm/codex-session-slot/v1", record))
            .map_err(|_| "slot record unavailable")?,
    );
    let salt = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, b"aterm/codex-slot-key/v1\0");
    let derived = salt.extract(identity.secret_bytes());
    let info: &[&[u8]] = &[b"session-assignment"];
    let expanded = derived
        .expand(info, ring::hkdf::HKDF_SHA256)
        .map_err(|_| "slot key unavailable")?;
    let mut raw = [0u8; 32];
    expanded
        .fill(&mut raw)
        .map_err(|_| "slot key unavailable")?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &raw);
    raw.zeroize();
    Ok(aterm_codec::hex::encode_lower(
        ring::hmac::sign(&key, bytes.as_slice()).as_ref(),
    ))
}

struct Opened {
    store: super::pending_native_store::Store,
    identity: age::Identity,
    directory: PathBuf,
    key: String,
    record: Option<Record>,
}

impl Opened {
    fn open(env: &Env, caller: &Caller, create: bool) -> Result<Option<Self>, String> {
        let (directory, home, key) = location(env, caller)?;
        let identity = machine(&home)?;
        let opened = if create {
            let parent = directory.parent().ok_or("slot directory unavailable")?;
            aterm_secrets::fsx::ensure_private_dir(parent)
                .map_err(|_| "slot directory unavailable")?;
            Some(super::pending_native_store::Store::open_owned(
                &directory, &home, MAX_RECORD,
            )?)
        } else {
            super::pending_native_store::Store::open_owned_existing(&directory, &home, MAX_RECORD)?
        };
        let Some((store, bytes)) = opened else {
            return Ok(None);
        };
        if store.recipient() != identity.recipient() {
            return Err("slot identity changed".into());
        }
        let record = bytes
            .map(|bytes| -> Result<Record, String> {
                let envelope: Envelope =
                    aterm_json::from_slice(bytes.as_slice()).map_err(|_| "slot record invalid")?;
                if !envelope.record.valid()
                    || envelope.record.caller != *caller
                    || !hex(&envelope.mac, 64)
                    || !aterm_digest::ct_eq(
                        envelope.mac.as_bytes(),
                        mac(&identity, &envelope.record)?.as_bytes(),
                    )
                {
                    return Err("slot record invalid".into());
                }
                Ok(envelope.record)
            })
            .transpose()?;
        let key = record
            .as_ref()
            .map(|record| slot_key(caller, &record.binding))
            .transpose()?
            .unwrap_or(key);
        Ok(Some(Self {
            store,
            identity,
            directory,
            key,
            record,
        }))
    }

    fn materialization_directory(&self) -> PathBuf {
        self.directory.join("credentials").join(&self.key)
    }

    fn save(&mut self, record: Record) -> Result<(), String> {
        if !record.valid() {
            return Err("slot record invalid".into());
        }
        let envelope = Envelope {
            mac: mac(&self.identity, &record)?,
            record: record.clone(),
        };
        let bytes =
            SecretBytes::new(aterm_json::to_vec(&envelope).map_err(|_| "slot record unavailable")?);
        self.store.save(bytes.as_slice())?;
        aterm_durable::Flush::Durable
            .dir(&self.directory)
            .map_err(|_| "slot durability unknown")?;
        self.store.unchanged()?;
        self.key = slot_key(&record.caller, &record.binding)?;
        self.record = Some(record);
        Ok(())
    }

    fn readback(
        &self,
        grant: &owner::AdmittedGrant<'_>,
        record: &Record,
    ) -> Result<SourceHandle, String> {
        let bytes = self.read_materialized(record)?;
        if bytes.as_slice() != grant.document() || record.revision != *grant.revision() {
            return Err("slot materialization changed".into());
        }
        let path = std::fs::canonicalize(self.materialization_directory().join("auth.json"))
            .map_err(|_| "slot credential unavailable")?;
        if path
            .to_str()
            .is_none_or(|raw| raw.len() > 4096 || raw.chars().any(char::is_control))
        {
            return Err("slot credential unavailable".into());
        }
        Ok(SourceHandle {
            binding: std::sync::Arc::new(grant.binding().clone()),
            slot: self.key.clone(),
            path,
            account: grant.identity().account.clone(),
            user: grant.identity().user.clone(),
            revision: record.revision.clone(),
            assignment_generation: record.assignment_generation,
            native_intent: record.source_authority.native_intent().map(str::to_owned),
        })
    }

    fn read_materialized(&self, record: &Record) -> Result<SecretBytes, String> {
        let directory = self.materialization_directory();
        let bytes = read_document(&directory)?;
        let marker = private_file::read_private(&directory, MARKER, 512)
            .map_err(|_| "slot association unavailable")?;
        if record.revision.digest
            != aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes.as_slice()))
            || marker != format!("ATERM-CODEX-AUTH-SLOT 1 {}\n", self.key).as_bytes()
        {
            return Err("slot materialization changed".into());
        }
        self.store.unchanged()?;
        Ok(bytes)
    }
}

fn read_document(directory: &Path) -> Result<SecretBytes, String> {
    let (file, metadata) = private_file::open_private(directory, "auth.json")
        .map_err(|_| "slot credential unavailable")?;
    if metadata.len() == 0 || metadata.len() > MAX_DOCUMENT as u64 {
        return Err("slot credential unavailable".into());
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(metadata.len() as usize));
    (&file)
        .take(MAX_DOCUMENT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "slot credential unavailable")?;
    if bytes.is_empty()
        || bytes.len() > MAX_DOCUMENT
        || !astream_sys::file_matches_path(&file, &directory.join("auth.json")).unwrap_or(false)
    {
        return Err("slot credential changed".into());
    }
    Ok(SecretBytes::new(std::mem::take(&mut *bytes)))
}

fn existing(env: &Env, caller: &Caller) -> Result<Record, String> {
    // This preliminary claim is dropped before entering account/grant custody.
    let opened = Opened::open(env, caller, false)?.ok_or("unknown session slot")?;
    opened.record.ok_or("unknown session slot".into())
}

fn auth_lock(
    env: &Env,
    caller: &Caller,
    binding: &owner::GrantBinding,
) -> Result<super::codex_app_login::AuthLock, String> {
    let directory = materialization_location(env, caller, binding)?;
    super::codex_app_login::AuthLock::take(&directory.join("auth.json"), Duration::from_secs(5))
        .map_err(|_| "slot credential busy")?
        .ok_or("slot credential busy".into())
}

/// Observe only a previously committed slot for this exact process. Discovery
/// never guesses a default account, creates a slot, or adopts a credential file.
pub fn source(env: &Env, caller: &Caller, operation: &str) -> Result<SourceProgress, SlotError> {
    if !caller.valid() || !hex(operation, 32) {
        return Err(SlotError::InvalidRequest);
    }
    let before = existing(env, caller).map_err(|_| SlotError::Unavailable)?;
    owner::with_current_published_grant(env, &before.binding, caller, |grant| {
        codex_assignment::with_current_assignment(
            env,
            caller,
            &before.binding,
            before.assignment_generation,
            &before.source_authority,
            || {
                let auth = auth_lock(env, caller, &before.binding)?;
                let opened = Opened::open(env, caller, false)?.ok_or("unknown session slot")?;
                if opened.record.as_ref() != Some(&before) {
                    return Err("session assignment changed".into());
                }
                if before.revision == *grant.revision() {
                    return opened.readback(&grant, &before).map(SourceProgress::Ready);
                }
                if grant.revision().generation <= before.revision.generation {
                    return Err("slot credential revision did not advance".into());
                }
                // Another consumer can refresh this same grant between install
                // and source lookup. Reconcile its already durable revision;
                // never exchange a token or change assignment during discovery.
                let mut record = before.clone();
                record.revision = grant.revision().clone();
                if opened.read_materialized(&before).is_err() {
                    // A prior reconciliation may have renamed the current
                    // document before its metadata write completed. Accept
                    // only the exact authenticated destination revision.
                    opened.read_materialized(&record)?;
                }
                let (source, _receipt) = materialize(opened, auth, &grant, record)?;
                Ok(SourceProgress::Ready(source))
            },
        )
    })
    .map_err(|_| SlotError::Unavailable)
}

fn prepare_materialization(
    env: &Env,
    caller: &Caller,
    binding: &owner::GrantBinding,
) -> Result<(Opened, super::codex_app_login::AuthLock), String> {
    let (directory, home, _) = location(env, caller)?;
    machine(&home)?;
    aterm_secrets::fsx::ensure_private_dir(directory.parent().ok_or("slot directory unavailable")?)
        .map_err(|_| "slot directory unavailable")?;
    aterm_secrets::fsx::ensure_private_dir(&directory).map_err(|_| "slot directory unavailable")?;
    let credentials = materialization_location(env, caller, binding)?;
    aterm_secrets::fsx::ensure_private_dir(
        credentials.parent().ok_or("slot directory unavailable")?,
    )
    .map_err(|_| "slot directory unavailable")?;
    aterm_secrets::fsx::ensure_private_dir(&credentials)
        .map_err(|_| "slot directory unavailable")?;
    let auth = auth_lock(env, caller, binding)?;
    let opened = Opened::open(env, caller, true)?.ok_or("slot unavailable")?;
    Ok((opened, auth))
}

/// Publish only after a real native assignment commit has acknowledged the
/// process-wide boundary. The receipt holds custody through the owner's ACK.
#[expect(
    dead_code,
    reason = "nothing calls this native custody step yet, tests included; its production caller lands with the custody RPC: drop this then"
)]
pub(super) fn install(
    env: &Env,
    grant: owner::AdmittedGrant<'_>,
    assignment: &codex_assignment::AssignmentCommit,
) -> Result<(SourceHandle, MaterializationReceipt), String> {
    let caller = assignment.caller();
    if !caller.valid()
        || assignment.binding() != grant.binding()
        || assignment.generation() == 0
        || !hex(assignment.operation(), 32)
        || !grant.revision().valid()
        || grant.document().is_empty()
        || grant.document().len() > MAX_DOCUMENT
    {
        return Err("invalid slot assignment".into());
    }
    let (opened, auth) = prepare_materialization(env, caller, grant.binding())?;
    let record = Record {
        schema: 2,
        caller: caller.clone(),
        binding: grant.binding().clone(),
        assignment_generation: assignment.generation(),
        source_authority: codex_assignment::SourceAuthority::Assignment {
            operation: assignment.operation().into(),
        },
        revision: grant.revision().clone(),
    };
    if !installation_allowed(
        opened.record.as_ref(),
        &record,
        assignment.previous_assignment_generation(),
    ) {
        return Err("session assignment changed".into());
    }
    materialize(opened, auth, &grant, record)
}

/// Retains the signed startup book and exact Slot claims through the native
/// constructor's final validation. Drop before the SDK reads its auth file.
pub(super) struct StartupReceipt {
    _materialization: MaterializationReceipt,
    _startup: codex_assignment::StartupClaim,
}

pub(super) fn initialize_startup(
    env: &Env,
    grant: &owner::AdmittedGrant<'_>,
    permit: &super::codex_source_registry::InitialSourcePermit,
) -> Result<(SourceHandle, StartupReceipt), String> {
    if grant.binding() != permit.binding()
        || !grant.revision().valid()
        || grant.document().is_empty()
        || grant.document().len() > MAX_DOCUMENT
    {
        return Err("invalid initial source".into());
    }
    let startup = codex_assignment::initialize_startup(env, grant, permit)?;
    let (source, receipt) = startup.with_current(grant, permit, || {
        let caller = permit.caller();
        let (opened, auth) = prepare_materialization(env, caller, grant.binding())?;
        let record = Record {
            schema: 2,
            caller: caller.clone(),
            binding: grant.binding().clone(),
            assignment_generation: 0,
            source_authority: codex_assignment::SourceAuthority::Startup {
                nonce: permit.startup_nonce().into(),
            },
            revision: grant.revision().clone(),
        };
        if !startup_installation_allowed(opened.record.as_ref(), &record) {
            return Err("initial source changed".into());
        }
        materialize(opened, auth, grant, record)
    })?;
    Ok((
        source,
        StartupReceipt {
            _materialization: receipt,
            _startup: startup,
        },
    ))
}

fn startup_installation_allowed(current: Option<&Record>, destination: &Record) -> bool {
    if !destination.valid() || destination.assignment_generation != 0 {
        return false;
    }
    current.is_none_or(|current| {
        current.valid()
            && current.caller == destination.caller
            && current.binding == destination.binding
            && current.source_authority == destination.source_authority
            && current.assignment_generation == 0
            && (current.revision == destination.revision
                || destination.revision.generation > current.revision.generation)
    })
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only tests reach this native custody step until its production caller lands; drop this then"
    )
)]
fn installation_allowed(current: Option<&Record>, destination: &Record, previous: u64) -> bool {
    if !destination.valid() || previous >= destination.assignment_generation {
        return false;
    }
    let Some(current) = current else {
        return previous == 0 && destination.assignment_generation > 0;
    };
    if current.caller != destination.caller {
        return false;
    }
    if current.assignment_generation == destination.assignment_generation {
        // A lost ACK or interrupted multi-file publication reconciles this exact
        // native assignment. The Slot record cannot mint the required proof.
        return current.binding == destination.binding
            && current.source_authority == destination.source_authority
            && (current.revision == destination.revision
                || destination.revision.generation > current.revision.generation);
    }
    current.assignment_generation == previous && destination.assignment_generation > previous
}

fn materialize(
    mut opened: Opened,
    auth: super::codex_app_login::AuthLock,
    grant: &owner::AdmittedGrant<'_>,
    record: Record,
) -> Result<(SourceHandle, MaterializationReceipt), String> {
    opened.key = slot_key(&record.caller, &record.binding)?;
    let directory = opened.materialization_directory();
    // Existing unresolved slot state must fail closed in Codex throughout this
    // multi-file publication, including a crash after the credential rename.
    let state = format!("ATERM-CODEX-AUTH-SLOT 1 {}\n", opened.key);
    private_file::write_private(&directory, "auth.json.aterm-slot-state", state.as_bytes())
        .map_err(|_| "slot publication unavailable")?;
    private_file::write_private(&directory, "auth.json", grant.document())
        .map_err(|_| "slot publication unavailable")?;
    private_file::write_private(&directory, MARKER, state.as_bytes())
        .map_err(|_| "slot publication unavailable")?;
    aterm_durable::Flush::Durable
        .dir(&directory)
        .map_err(|_| "slot publication durability unknown")?;
    opened.save(record.clone())?;
    let source = opened.readback(grant, &record)?;
    let receipt = MaterializationReceipt {
        caller: record.caller,
        slot: opened.key,
        revision: record.revision,
        _auth: auth,
        _store: opened.store,
    };
    Ok((source, receipt))
}

pub fn refresh(
    env: &Env,
    caller: &Caller,
    request: &RefreshRequest,
) -> Result<RefreshProgress, SlotError> {
    if !caller.valid() || !hex(&request.operation, 32) || !hex(&request.expected_digest, 64) {
        return Err(SlotError::InvalidRequest);
    }
    let before = existing(env, caller).map_err(|_| SlotError::Unavailable)?;
    if slot_key(caller, &before.binding).map_err(|_| SlotError::InvalidRequest)? != request.slot {
        return Err(SlotError::InvalidRequest);
    }
    let mut admitted_request = RefreshRequest::new(
        request.slot.clone(),
        request.operation.clone(),
        request.expected_digest.clone(),
    );
    admitted_request.authority = Some(RefreshAuthority {
        binding: before.binding.clone(),
        assignment_generation: before.assignment_generation,
        source_authority: before.source_authority.clone(),
        materialized_revision: before.revision.clone(),
    });
    let result =
        owner::with_refreshed_owner(env, &before.binding, caller, &admitted_request, |grant| {
            codex_assignment::with_current_assignment(
                env,
                caller,
                &before.binding,
                before.assignment_generation,
                &before.source_authority,
                || {
                    let auth = auth_lock(env, caller, &before.binding)?;
                    let opened = Opened::open(env, caller, false)?.ok_or("unknown session slot")?;
                    if opened.record.as_ref() != Some(&before) {
                        return Err("session assignment changed".into());
                    }
                    let mut record = before.clone();
                    record.revision = grant.revision().clone();
                    materialize(opened, auth, &grant, record)
                },
            )
        });
    match result {
        Ok((_, revision)) => Ok(RefreshProgress::Ready(revision)),
        Err(owner::OwnerRefreshError::Unavailable) => Err(SlotError::Unavailable),
        Err(owner::OwnerRefreshError::Retaining(inner)) => {
            Ok(RefreshProgress::Retaining(RefreshRetention { inner }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller() -> Caller {
        Caller {
            sid: "s-slot-fixture".into(),
            pid: 42,
            birth: 123,
        }
    }

    fn fixture(key: bool) -> (aterm_tempfile::TempDir, Env) {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
        if key {
            client::machine_identity_as(
                &client::Paths::locate_for_home(&home),
                aterm_secrets::Making {
                    test_binary: true,
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let env = Env {
            state: root.path().join("state"),
            aterm_state: Some(root.path().join("aterm")),
            cwd: home.clone(),
            home: Some(home),
            claude_dir: None,
            claude_config_unusable: None,
            claude_profile: None,
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            codex_home: None,
            codex_login: None,
            agy_conversation: None,
            now: 1_790_000_000,
            sid: "s-slot-fixture".into(),
            sock: None,
            config: None,
            claude_tmp: None,
            claude_running: None,
            tz_s: 0,
        };
        (root, env)
    }

    // Persisted GrantBinding is data, never an admitted grant. This fixture can
    // exercise slot authentication but cannot construct AdmittedGrant or install.
    fn record(caller: Caller) -> Record {
        let binding: owner::GrantBinding = aterm_json::from_value(aterm_json::json!({
            "request":{"id":"a".repeat(32),"caller":"b".repeat(64),"label":"fixture","revision":"c".repeat(64)},
            "reference":{"operation":"a".repeat(32),"capability":"d".repeat(64),
                "issuance_digest":"e".repeat(64),"producer_mac":"f".repeat(64)},
            "generation":1
        })).unwrap();
        Record {
            schema: 2,
            caller,
            binding,
            assignment_generation: 1,
            source_authority: codex_assignment::SourceAuthority::Assignment {
                operation: "1".repeat(32),
            },
            revision: Revision {
                generation: 1,
                digest: "2".repeat(64),
            },
        }
    }

    #[test]
    fn startup_metadata_requires_its_own_nonce_and_cannot_replace_an_assignment() {
        // Record-shape checks only. These values cannot construct a native
        // InitialSourcePermit, AdmittedGrant or StartupClaim.
        let assigned = record(caller());
        let mut startup = assigned.clone();
        startup.assignment_generation = 0;
        assert!(!startup.valid());
        startup.source_authority = codex_assignment::SourceAuthority::Startup {
            nonce: "7".repeat(32),
        };
        assert!(startup.valid());
        assert!(startup_installation_allowed(None, &startup));
        assert!(startup_installation_allowed(Some(&startup), &startup));
        assert!(!startup_installation_allowed(Some(&assigned), &startup));
        let mut later = startup.clone();
        later.revision.generation += 1;
        later.revision.digest = "8".repeat(64);
        assert!(startup_installation_allowed(Some(&startup), &later));
        assert!(!startup_installation_allowed(Some(&later), &startup));
        later = startup.clone();
        later.revision.digest = "8".repeat(64);
        assert!(!startup_installation_allowed(Some(&startup), &later));
        later = startup.clone();
        later.source_authority = codex_assignment::SourceAuthority::Startup {
            nonce: "9".repeat(32),
        };
        assert!(!startup_installation_allowed(Some(&startup), &later));
        assert!(installation_allowed(Some(&startup), &assigned, 0));
        assert!(startup.source_authority.native_intent().is_none());
        assert_eq!(
            assigned.source_authority.native_intent(),
            Some("1".repeat(32).as_str())
        );
        startup.revision.generation = 0;
        assert!(!startup.valid());
    }

    #[test]
    fn slot_keys_bind_full_process_lineage_and_are_bounded_utf8_keys() {
        let base = caller();
        let binding = record(base.clone()).binding;
        let key = slot_key(&base, &binding).unwrap();
        assert!(key.len() <= 256 && key.len().is_multiple_of(2) && hex(&key, key.len()));
        for alternate in [
            Caller {
                sid: "s-other".into(),
                ..base.clone()
            },
            Caller {
                pid: 43,
                ..base.clone()
            },
            Caller {
                birth: 124,
                ..base.clone()
            },
        ] {
            assert_ne!(key, slot_key(&alternate, &binding).unwrap());
        }
        assert!(
            slot_key(
                &Caller {
                    sid: "../../escape".into(),
                    ..base
                },
                &binding
            )
            .is_err()
        );
    }

    #[test]
    fn credential_revision_domains_do_not_share_a_materialization_or_slot_key() {
        let (_root, env) = fixture(true);
        let caller = caller();
        let first = record(caller.clone()).binding;
        let mut second = first.clone();
        second.generation += 1;
        let first_key = slot_key(&caller, &first).unwrap();
        let second_key = slot_key(&caller, &second).unwrap();
        assert_ne!(first_key, second_key);
        assert_eq!(first_key, slot_key(&caller, &first).unwrap());
        let first_path = materialization_location(&env, &caller, &first).unwrap();
        let second_path = materialization_location(&env, &caller, &second).unwrap();
        assert_ne!(first_path, second_path);
        aterm_secrets::fsx::ensure_private_dir(&first_path).unwrap();
        aterm_secrets::fsx::ensure_private_dir(&second_path).unwrap();
        private_file::write_private(&first_path, "auth.json", b"first-grant-revision-14").unwrap();
        private_file::write_private(&second_path, "auth.json", b"second-grant-revision-1").unwrap();
        assert_eq!(
            read_document(&first_path).unwrap().as_slice(),
            b"first-grant-revision-14"
        );
        assert_eq!(
            read_document(&second_path).unwrap().as_slice(),
            b"second-grant-revision-1"
        );
    }

    #[test]
    fn lost_ack_reconciles_only_the_exact_assignment_without_revision_rollback() {
        let current = record(caller());
        assert!(installation_allowed(None, &current, 0));
        assert!(installation_allowed(Some(&current), &current, 0));
        assert!(!installation_allowed(Some(&current), &current, 1));
        let mut destination = current.clone();
        destination.revision.generation += 1;
        destination.revision.digest = "4".repeat(64);
        assert!(installation_allowed(Some(&current), &destination, 0));
        assert!(!installation_allowed(Some(&destination), &current, 0));
        destination = current.clone();
        destination.revision.digest = "4".repeat(64);
        assert!(!installation_allowed(Some(&current), &destination, 0));
        destination = current.clone();
        destination.source_authority = codex_assignment::SourceAuthority::Assignment {
            operation: "5".repeat(32),
        };
        assert!(!installation_allowed(Some(&current), &destination, 0));
        destination = current.clone();
        destination.binding.generation += 1;
        assert!(!installation_allowed(Some(&current), &destination, 0));
        destination = current.clone();
        destination.caller.birth += 1;
        assert!(!installation_allowed(Some(&current), &destination, 0));
        destination = current.clone();
        destination.assignment_generation += 1;
        destination.source_authority = codex_assignment::SourceAuthority::Assignment {
            operation: "6".repeat(32),
        };
        assert!(installation_allowed(Some(&current), &destination, 1));
        assert!(!installation_allowed(Some(&current), &destination, 0));
    }

    #[test]
    fn unknown_source_never_creates_a_slot_or_machine_identity() {
        for key in [false, true] {
            let (_root, env) = fixture(key);
            let caller = caller();
            let (directory, home, _) = location(&env, &caller).unwrap();
            assert!(source(&env, &caller, &"a".repeat(32)).is_err());
            assert!(!directory.exists());
            assert_eq!(machine(&home).is_ok(), key);
        }
    }

    #[cfg(unix)]
    #[test]
    fn credential_readback_refuses_links_loose_permissions_and_oversized_files() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = aterm_tempfile::tempdir().unwrap();
        let directory = root.path().join("materialization");
        aterm_secrets::fsx::ensure_private_dir(&directory).unwrap();
        let path = directory.join("auth.json");
        private_file::write_private(&directory, "auth.json", b"private-document").unwrap();
        assert_eq!(
            read_document(&directory).unwrap().as_slice(),
            b"private-document"
        );
        std::fs::hard_link(&path, directory.join("alias")).unwrap();
        assert!(read_document(&directory).is_err());
        std::fs::remove_file(directory.join("alias")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_document(&directory).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::rename(&path, directory.join("original")).unwrap();
        symlink(directory.join("original"), &path).unwrap();
        assert!(read_document(&directory).is_err());
        std::fs::remove_file(&path).unwrap();
        private_file::write_private(&directory, "auth.json", &vec![b'x'; MAX_DOCUMENT + 1])
            .unwrap();
        assert!(read_document(&directory).is_err());
    }

    #[test]
    fn public_recipient_reencryption_cannot_retarget_an_authenticated_slot() {
        let (_root, env) = fixture(true);
        let caller = caller();
        let mut opened = Opened::open(&env, &caller, true).unwrap().unwrap();
        let original = record(caller.clone());
        opened.save(original.clone()).unwrap();
        let directory = opened.directory.clone();
        let recipient = opened.identity.recipient();
        let old_mac = mac(&opened.identity, &original).unwrap();
        drop(opened);
        assert!(
            Opened::open(&env, &caller, false)
                .unwrap()
                .unwrap()
                .record
                .as_ref()
                == Some(&original)
        );
        let mut forged = original;
        forged.assignment_generation += 1;
        forged.source_authority = codex_assignment::SourceAuthority::Assignment {
            operation: "3".repeat(32),
        };
        let plaintext = SecretBytes::new(
            aterm_json::to_vec(&Envelope {
                record: forged,
                mac: old_mac,
            })
            .unwrap(),
        );
        let ciphertext = age::encrypt(&[recipient], plaintext.as_slice()).unwrap();
        aterm_secrets::fsx::write_atomic(&directory.join("candidate.age"), &ciphertext, 0o600)
            .unwrap();
        assert!(Opened::open(&env, &caller, false).is_err());
    }
}
