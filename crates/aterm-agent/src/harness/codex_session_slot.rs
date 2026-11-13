// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-process credential materializations. A slot never creates a grant or an
//! assignment: both must arrive as current, typed native owner evidence.

use super::native_custody_platform as platform;
use super::native_custody_platform as private_file;
use super::{accounts, cli::Env, codex_assignment, codex_grant_owner as owner};
#[cfg(all(test, unix))]
use aterm_secrets::client;
use aterm_secrets::{SecretBytes, age};
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

#[path = "codex_session_slot_use_guard.rs"]
mod use_guard;
pub(in crate::harness) use use_guard::source_for_preload;
pub use use_guard::{refresh_for_use, source_for_use};

/// Descriptive identities captured under actual MAC-authenticated Book and
/// Slot claims. Equality joins physical aliases; neither path nor this value
/// grants permission to load credentials or declare another cohort disjoint.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct SourceNamespaces {
    book: codex_assignment::BookNamespace,
    slot: platform::PhysicalIdentity,
}
impl SourceNamespaces {
    fn capture(
        book: &codex_assignment::BookNamespace,
        store: &platform::Store,
    ) -> Result<Self, String> {
        #[cfg(unix)]
        {
            Ok(Self {
                book: book.clone(),
                slot: store.physical_identity().map_err(str::to_owned)?,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = book;
            match *store {}
        }
    }

    pub(super) fn current(&self) -> bool {
        self.book.current() && self.slot.current()
    }
    pub(super) fn shares_storage(&self, other: &Self) -> bool {
        self.book == other.book || self.slot == other.slot
    }
    pub(super) fn book(&self) -> &codex_assignment::BookNamespace {
        &self.book
    }
}

pub struct SourceHandle {
    binding: std::sync::Arc<owner::GrantBinding>,
    slot: String,
    path: PathBuf,
    account: String,
    user: String,
    revision: Revision,
    assignment_generation: u64,
    source_authority: codex_assignment::SourceAuthority,
    namespaces: Option<std::sync::Arc<SourceNamespaces>>,
    policy: Option<Box<codex_assignment::ProviderPolicy>>,
}

impl std::fmt::Debug for SourceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceHandle(redacted)")
    }
}

impl SourceHandle {
    /// Descriptive policy from the same admitted document used by Slot readback.
    /// Absence cannot authorize a policy preview or a provider request.
    pub fn provider_policy(&self) -> Option<&codex_assignment::ProviderPolicy> {
        self.policy.as_deref()
    }
    pub(super) fn namespaces(&self) -> Option<&SourceNamespaces> {
        self.namespaces.as_deref()
    }
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
        self.source_authority.native_intent()
    }
    pub(super) fn source_authority(&self) -> &codex_assignment::SourceAuthority {
        &self.source_authority
    }
}

/// This receipt retains both the materialization flock and metadata claim until
/// the owner has committed its acknowledgement. It has no data constructor.
pub(super) struct MaterializationReceipt {
    caller: Caller,
    slot: String,
    revision: Revision,
    _auth: super::codex_app_login::AuthLock,
    _store: platform::Store,
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
    platform::machine_identity(home)
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
    store: platform::Store,
    identity: age::Identity,
    directory: PathBuf,
    key: String,
    record: Option<Record>,
}

impl Opened {
    fn open(env: &Env, caller: &Caller, create: bool) -> Result<Option<Self>, String> {
        platform::require_available()?;
        let (directory, home, key) = location(env, caller)?;
        let identity = machine(&home)?;
        let opened = if create {
            let parent = directory.parent().ok_or("slot directory unavailable")?;
            platform::ensure_private_dir(parent).map_err(|_| "slot directory unavailable")?;
            Some(platform::Store::open_owned(&directory, &home, MAX_RECORD)?)
        } else {
            platform::Store::open_owned_existing(&directory, &home, MAX_RECORD)?
        };
        let Some((store, bytes)) = opened else {
            return Ok(None);
        };
        Self::from_store(store, bytes, identity, directory, key, caller).map(Some)
    }

    fn from_store(
        store: platform::Store,
        bytes: Option<SecretBytes>,
        identity: age::Identity,
        directory: PathBuf,
        key: String,
        caller: &Caller,
    ) -> Result<Self, String> {
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
        Ok(Self {
            store,
            identity,
            directory,
            key,
            record,
        })
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
        book_namespace: &codex_assignment::BookNamespace,
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
            policy: codex_assignment::ProviderPolicy::capture(grant)
                .ok()
                .map(Box::new),
            binding: std::sync::Arc::new(grant.binding().clone()),
            slot: self.key.clone(),
            path,
            account: grant.identity().account.clone(),
            user: grant.identity().user.clone(),
            revision: record.revision.clone(),
            assignment_generation: record.assignment_generation,
            source_authority: record.source_authority.clone(),
            namespaces: Some(std::sync::Arc::new(SourceNamespaces::capture(
                book_namespace,
                &self.store,
            )?)),
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
    source_inner(env, caller, operation, None)
}

/// A known registered member may preload only its captured immutable scope.
/// The caller retains its actual registry/App admission; this adds no repair,
/// bootstrap, membership or target authority.
pub(super) fn source_matching_registered(
    env: &Env,
    caller: &Caller,
    operation: &str,
    expected: &SourceHandle,
) -> Result<SourceProgress, SlotError> {
    source_inner(env, caller, operation, Some(expected))
}

/// Reauthenticate existing signed scope without owner admission, token reads,
/// refresh, materialization or inferred bootstrap authority.
pub(super) fn authenticate_source_scope(
    env: &Env,
    caller: &Caller,
    expected: &SourceHandle,
) -> Result<(), String> {
    let namespaces = expected.namespaces().ok_or("source namespace is unknown")?;
    if !namespaces.current() {
        return Err("source namespace changed".into());
    }
    codex_assignment::with_current_assignment_namespace(
        env,
        caller,
        expected.binding(),
        expected.assignment_generation(),
        expected.source_authority(),
        |book| {
            if book != namespaces.book() {
                return Err("source Book namespace changed".into());
            }
            let opened = Opened::open(env, caller, false)?.ok_or("source Slot unavailable")?;
            let record = opened.record.as_ref().ok_or("source Slot unavailable")?;
            use_guard::check_record(env, caller, record, expected)
                .map_err(|_| "source Slot changed")?;
            if opened.store.physical_identity().map_err(str::to_owned)? != namespaces.slot {
                return Err("source Slot namespace changed".into());
            }
            opened.store.unchanged().map_err(str::to_owned)
        },
    )
}

/// Authenticate a member's captured Book and Slot without resolving a new
/// default, reading token bytes or materializing any revision.
pub(super) fn authenticate_captured_namespaces(
    caller: &Caller,
    expected: &SourceHandle,
    gate: &super::codex_source_gate::SourceGateIdentity,
) -> Result<(), String> {
    let namespaces = expected.namespaces().ok_or("source namespace unknown")?;
    let home = gate.existing_home().ok_or("source home changed")?;
    if !namespaces.current() {
        return Err("source namespace changed".into());
    }
    codex_assignment::authenticate_namespace(
        &home,
        namespaces.book(),
        caller,
        expected.binding(),
        expected.assignment_generation(),
        expected.source_authority(),
        || {
            let directory = namespaces.slot.directory();
            let identity = machine(&home)?;
            let (store, bytes) =
                platform::Store::open_owned_existing(directory, &home, MAX_RECORD)?
                    .ok_or("source Slot unavailable")?;
            if store.physical_identity().map_err(str::to_owned)? != namespaces.slot {
                return Err("source Slot namespace changed".into());
            }
            let opened = Opened::from_store(
                store,
                bytes,
                identity,
                directory.into(),
                expected.slot.clone(),
                caller,
            )?;
            let record = opened.record.as_ref().ok_or("source Slot unavailable")?;
            if record.binding != *expected.binding()
                || record.assignment_generation != expected.assignment_generation
                || record.source_authority != expected.source_authority
                || opened.key != expected.slot
                || record.revision.generation < expected.revision.generation
                || (record.revision.generation == expected.revision.generation
                    && record.revision.digest != expected.revision.digest)
                || std::fs::canonicalize(opened.materialization_directory().join("auth.json"))
                    .map_err(|_| "source file unavailable")?
                    != expected.path
            {
                return Err("source Slot authority changed".into());
            }
            opened.store.unchanged().map_err(str::to_owned)
        },
    )
}

fn validate_expected_namespaces(
    env: &Env,
    caller: &Caller,
    expected: &SourceHandle,
) -> Result<(), SlotError> {
    let Some(namespaces) = expected.namespaces() else {
        return Ok(());
    };
    if !namespaces.current() {
        return Err(SlotError::Unavailable);
    }
    codex_assignment::with_current_assignment_namespace(
        env,
        caller,
        expected.binding(),
        expected.assignment_generation(),
        expected.source_authority(),
        |actual| {
            if actual != namespaces.book() {
                return Err("source Book namespace changed".into());
            }
            Ok(())
        },
    )
    .map_err(|_| SlotError::Unavailable)
}

fn validate_expected_in_namespace(
    env: &Env,
    caller: &Caller,
    expected: &SourceHandle,
    namespace: &RefreshNamespace,
) -> Result<(), SlotError> {
    validate_expected_in_read_namespace(
        env,
        caller,
        expected,
        SourceReadNamespace::RefreshingUse(namespace),
    )
}

fn validate_expected_in_read_namespace(
    env: &Env,
    caller: &Caller,
    expected: &SourceHandle,
    namespace: SourceReadNamespace<'_>,
) -> Result<(), SlotError> {
    namespace
        .with_namespace(
            env,
            caller,
            expected.binding(),
            expected.assignment_generation(),
            expected.source_authority(),
            |actual| {
                if expected
                    .namespaces()
                    .is_some_and(|captured| !captured.current() || captured.book() != actual)
                {
                    return Err("source physical namespace changed".into());
                }
                Ok(())
            },
        )
        .map_err(|_| SlotError::Unavailable)
}

fn source_inner(
    env: &Env,
    caller: &Caller,
    operation: &str,
    expected: Option<&SourceHandle>,
) -> Result<SourceProgress, SlotError> {
    source_inner_using_namespace(env, caller, operation, expected, None)
}

fn source_inner_using_namespace(
    env: &Env,
    caller: &Caller,
    operation: &str,
    expected: Option<&SourceHandle>,
    namespace: Option<SourceReadNamespace<'_>>,
) -> Result<SourceProgress, SlotError> {
    if namespace.is_some() && expected.is_none() {
        return Err(SlotError::InvalidRequest);
    }
    if !caller.valid() || !hex(operation, 32) {
        return Err(SlotError::InvalidRequest);
    }
    let before = existing(env, caller).map_err(|_| SlotError::Unavailable)?;
    if let Some(expected) = expected {
        use_guard::check_record(env, caller, &before, expected)?;
        if let Some(namespace) = namespace {
            validate_expected_in_read_namespace(env, caller, expected, namespace)?;
        } else {
            validate_expected_namespaces(env, caller, expected)?;
        }
    } else {
        // Legacy/uncontextual Source may never enter a pending B owner. The
        // claimed signed assignment is checked and released before accounts.
        codex_assignment::with_current_assignment(
            env,
            caller,
            &before.binding,
            before.assignment_generation,
            &before.source_authority,
            || Ok(()),
        )
        .map_err(|_| SlotError::Unavailable)?;
    }
    #[cfg(test)]
    use_guard::note_owner_entry();
    let result = owner::with_current_published_grant(env, &before.binding, caller, |grant| {
        let inspect = |book_namespace: &codex_assignment::BookNamespace| {
            let auth = auth_lock(env, caller, &before.binding)?;
            let opened = Opened::open(env, caller, false)?.ok_or("unknown session slot")?;
            if opened.record.as_ref() != Some(&before) {
                return Err("session assignment changed".into());
            }
            if before.revision == *grant.revision() {
                return opened
                    .readback(&grant, &before, book_namespace)
                    .map(SourceProgress::Ready);
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
            let (source, _receipt) = materialize(opened, auth, &grant, record, book_namespace)?;
            Ok(SourceProgress::Ready(source))
        };
        if let Some(namespace) = namespace {
            namespace.with_namespace(
                env,
                caller,
                &before.binding,
                before.assignment_generation,
                &before.source_authority,
                inspect,
            )
        } else {
            codex_assignment::with_current_assignment_namespace(
                env,
                caller,
                &before.binding,
                before.assignment_generation,
                &before.source_authority,
                inspect,
            )
        }
    })
    .map_err(|_| SlotError::Unavailable)?;
    if let Some(expected) = expected {
        let after = existing(env, caller).map_err(|_| SlotError::Unavailable)?;
        use_guard::check_record(env, caller, &after, expected)?;
        use_guard::check_source(&result, expected)?;
    }
    Ok(result)
}

fn prepare_materialization(
    env: &Env,
    caller: &Caller,
    binding: &owner::GrantBinding,
) -> Result<(Opened, super::codex_app_login::AuthLock), String> {
    let (directory, home, _) = location(env, caller)?;
    machine(&home)?;
    platform::ensure_private_dir(directory.parent().ok_or("slot directory unavailable")?)
        .map_err(|_| "slot directory unavailable")?;
    platform::ensure_private_dir(&directory).map_err(|_| "slot directory unavailable")?;
    let credentials = materialization_location(env, caller, binding)?;
    platform::ensure_private_dir(credentials.parent().ok_or("slot directory unavailable")?)
        .map_err(|_| "slot directory unavailable")?;
    platform::ensure_private_dir(&credentials).map_err(|_| "slot directory unavailable")?;
    let auth = auth_lock(env, caller, binding)?;
    let opened = Opened::open(env, caller, true)?.ok_or("slot unavailable")?;
    Ok((opened, auth))
}

/// Publish only after a real native assignment commit has acknowledged the
/// process-wide boundary. The receipt holds custody through the owner's ACK.
pub(super) fn install(
    env: &Env,
    grant: &owner::AdmittedGrant<'_>,
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
    assignment.with_namespace(|book_namespace| {
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
        materialize(opened, auth, grant, record, book_namespace)
    })
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
    let (source, receipt) = startup.with_current(grant, permit, |book_namespace| {
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
        materialize(opened, auth, grant, record, book_namespace)
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
    book_namespace: &codex_assignment::BookNamespace,
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
    let source = opened.readback(grant, &record, book_namespace)?;
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
    scope: &super::account_work::Scope,
) -> Result<RefreshProgress, SlotError> {
    refresh_inner(env, caller, request, scope, None)
}

fn refresh_inner(
    env: &Env,
    caller: &Caller,
    request: &RefreshRequest,
    scope: &super::account_work::Scope,
    expected: Option<&SourceHandle>,
) -> Result<RefreshProgress, SlotError> {
    refresh_inner_using_namespace(env, caller, request, scope, expected, None)
}

fn refresh_inner_using_namespace(
    env: &Env,
    caller: &Caller,
    request: &RefreshRequest,
    scope: &super::account_work::Scope,
    expected: Option<&SourceHandle>,
    namespace: Option<&RefreshNamespace>,
) -> Result<RefreshProgress, SlotError> {
    if !caller.valid() || !hex(&request.operation, 32) || !hex(&request.expected_digest, 64) {
        return Err(SlotError::InvalidRequest);
    }
    let before = existing(env, caller).map_err(|_| SlotError::Unavailable)?;
    if let Some(expected) = expected {
        use_guard::check_record(env, caller, &before, expected)?;
        if let Some(namespace) = namespace {
            validate_expected_in_namespace(env, caller, expected, namespace)?;
        } else {
            validate_expected_namespaces(env, caller, expected)?;
        }
    } else {
        codex_assignment::with_current_assignment(
            env,
            caller,
            &before.binding,
            before.assignment_generation,
            &before.source_authority,
            || Ok(()),
        )
        .map_err(|_| SlotError::Unavailable)?;
    }
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
    #[cfg(test)]
    use_guard::note_owner_entry();
    let materialize_current = |grant: owner::AdmittedGrant<'_>| {
        let materialize_in = |book_namespace: &codex_assignment::BookNamespace| {
            let auth = auth_lock(env, caller, &before.binding)?;
            let opened = Opened::open(env, caller, false)?.ok_or("unknown session slot")?;
            if opened.record.as_ref() != Some(&before) {
                return Err("session assignment changed".into());
            }
            let mut record = before.clone();
            record.revision = grant.revision().clone();
            materialize(opened, auth, &grant, record, book_namespace)
        };
        if let Some(namespace) = namespace {
            namespace.with_namespace(
                env,
                caller,
                &before.binding,
                before.assignment_generation,
                &before.source_authority,
                materialize_in,
            )
        } else {
            codex_assignment::with_current_assignment_namespace(
                env,
                caller,
                &before.binding,
                before.assignment_generation,
                &before.source_authority,
                materialize_in,
            )
        }
    };
    let result = if let Some(namespace) = namespace {
        owner::with_refreshed_owner_in_namespace(
            env,
            &before.binding,
            caller,
            &admitted_request,
            namespace,
            materialize_current,
        )
    } else {
        owner::with_refreshed_owner(
            env,
            &before.binding,
            caller,
            &admitted_request,
            scope,
            materialize_current,
        )
    };
    match result {
        Ok((_, revision)) => {
            if let Some(expected) = expected {
                let after = existing(env, caller).map_err(|_| SlotError::Unavailable)?;
                use_guard::check_record(env, caller, &after, expected)?;
            }
            Ok(RefreshProgress::Ready(revision))
        }
        Err(owner::OwnerRefreshError::Unavailable) => Err(SlotError::Unavailable),
        Err(owner::OwnerRefreshError::Retaining(inner)) => {
            Ok(RefreshProgress::Retaining(RefreshRetention { inner }))
        }
    }
}

#[cfg(all(test, unix))]
#[path = "codex_session_slot_metadata_fixture.rs"]
pub(in crate::harness) mod metadata_fixture;

#[path = "codex_session_slot_target.rs"]
mod target;
pub(super) use target::{
    preload_installed, preload_normalized, validate_installed, validate_normalized,
    validate_target_ack, validate_target_snapshot,
};
#[path = "codex_session_slot_namespace.rs"]
mod namespace;
pub(in crate::harness) use namespace::RefreshNamespace;
#[path = "codex_session_slot_read_namespace.rs"]
mod read_namespace;
use read_namespace::SourceReadNamespace;

#[cfg(test)]
#[path = "codex_session_slot_tests.rs"]
mod tests;
