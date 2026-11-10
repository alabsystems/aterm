// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Encrypted custody of an enrollment's irreversible OAuth response. A retained
//! response is not a published account, independent grant, or session assignment.
//! Only the successful exchange producer can supply verified response evidence.

use super::native_custody_platform as platform;
use super::{cli::Env, codex_app_login, codex_enrollment, codex_renewal::Identity};
#[cfg(all(test, unix))]
use aterm_secrets::client;
use aterm_secrets::{SecretBytes, age};
#[cfg(all(test, unix))]
use aterm_uds::private_file;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

const MAX_DOCUMENT: usize = 16 * 1024;
// A completed unsupported response must still survive. This is the native
// reader's existing document bound, not permission to materialize a large slot.
const MAX_RETAINED_DOCUMENT: usize = 2 * 1024 * 1024;
const MAX_ENVELOPE: usize = 4 * 1024 * 1024;
const MAX_PENDING: usize = 256;

/// Owned physical obstruction used only by the isolated pending-custody fixture.
#[cfg(all(unix, any(test, debug_assertions)))]
pub(super) struct PendingFixtureRestore {
    directory: PathBuf,
    held: PathBuf,
}

#[cfg(all(unix, any(test, debug_assertions)))]
impl PendingFixtureRestore {
    pub(super) fn restore(&mut self) -> Result<(), String> {
        if !self.held.exists() {
            return Ok(());
        }
        std::fs::remove_dir(&self.directory).map_err(|_| "fixture replacement removal failed")?;
        std::fs::rename(&self.held, &self.directory)
            .map_err(|_| "fixture custody restore failed".to_owned())
    }
}

#[cfg(all(unix, any(test, debug_assertions)))]
pub(super) fn pending_custody_fixture(
    env: &Env,
    request: &codex_enrollment::Request,
    stage: &Path,
    original: &super::account_work::Scope,
    turn: std::sync::Arc<super::native_enrollment_admission::NativeBrowserTurn>,
    admission: super::codex_native_row::NativeRowAdmission,
) -> Result<(CaptureOutcome, PendingFixtureRestore), String> {
    let deadline = super::wake_deadline::WakeDeadline::after(super::login_drive::HANG);
    let policy = admission.staging().policy.clone();
    let mut pending = prepare(env, request, stage, &policy)?
        .with_native_scope(env, Some(turn.clone()))?
        .with_row_admission(admission)?;
    turn.enter_exchange(deadline, Some(original))?;
    drop(turn);
    pending.begin_exchange()?;
    let directory = pending.store.held()?.directory.clone();
    let held = directory.with_extension("fixture-held");
    std::fs::rename(&directory, &held).map_err(|_| "fixture custody move failed")?;
    platform::ensure_private_dir(&directory).map_err(|_| "fixture replacement failed")?;
    pending.capture_response(
        br#"{"tokens":{"access_token":"isolated-fixture-only"}}"#,
        // Deliberately unsupported body: storage/lifetime custody is real,
        // but no verified Code grant or publication authority is fabricated.
        None,
        None,
    );
    let outcome = pending.settle_response();
    if !matches!(&outcome, CaptureOutcome::Pending(_)) {
        return Err("physical replacement did not retain pending custody".into());
    }
    Ok((outcome, PendingFixtureRestore { directory, held }))
}

#[path = "codex_grant_catalogue.rs"]
mod catalogue;
pub(super) use catalogue::CatalogueTransition;

#[path = "codex_grant_published_source.rs"]
mod published_source;

#[path = "codex_grant_refresh.rs"]
mod refresh;
pub(in crate::harness) use refresh::NativeUsageGrant;
#[cfg(all(test, unix))]
pub(super) use refresh::initialize_revision;
pub use refresh::{
    NativeUsageContinuation, NativeUsageDeferred, NativeUsageDiscoveryCursor, NativeUsageOutcome,
    NativeUsageRequest, NativeUsageRetention, NativeUsageSettlement, NativeUsageSourceLease,
    discover_native_usage_deferred, read_native_usage, read_native_usage_observed,
};
pub(super) use refresh::{
    OwnedRefreshRetention, OwnerRefreshContinuation, OwnerRefreshError, OwnerWitnessTransition,
    with_refreshed_owner, with_refreshed_owner_in_namespace,
};
#[cfg(all(unix, any(test, debug_assertions)))]
pub(super) use refresh::{advance_source_proof, reconcile_source_proof};

/// A baseline initializer's borrowed fresh-publication authority. There is no
/// constructor from an Applied record, copied document or retained reference.
/// Its mint point must be the actual generation/publication transaction while
/// prospective source and launch custody remain held.
pub(super) struct FreshActivationGuard<'a> {
    binding: &'a GrantBinding,
    identity: &'a Identity,
    document: &'a [u8],
}

impl std::fmt::Debug for FreshActivationGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FreshActivationGuard(redacted)")
    }
}

impl FreshActivationGuard<'_> {
    pub(super) fn binding(&self) -> &GrantBinding {
        self.binding
    }
    pub(super) fn identity(&self) -> &Identity {
        self.identity
    }
    pub(super) fn document(&self) -> &[u8] {
        self.document
    }
}

/// Initialize the mutable owner revision while the live publication transaction
/// holds the exact authenticated response and its derived canonical binding.
fn initialize_fresh_revision(
    env: &Env,
    binding: &GrantBinding,
    identity: &Identity,
    document: &[u8],
) -> Result<OwnerWitnessTransition, String> {
    catalogue::require_publication(env, binding, identity, document)?;
    refresh::initialize_revision(
        env,
        &FreshActivationGuard {
            binding,
            identity,
            document,
        },
    )
}

/// A borrowed admission under the account, owner and native auth claims. There
/// is no constructor from JSON, a slot record, or an observed credential path.
pub(super) struct AdmittedGrant<'a> {
    binding: &'a GrantBinding,
    identity: &'a Identity,
    revision: &'a super::codex_session_slot::Revision,
    document: &'a [u8],
    canonical_auth: &'a Path,
    anchor: codex_enrollment::OwnerAnchorWriter<'a>,
}

impl std::fmt::Debug for AdmittedGrant<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AdmittedGrant(redacted)")
    }
}

impl<'a> AdmittedGrant<'a> {
    pub(super) fn binding(&self) -> &GrantBinding {
        self.binding
    }
    pub(super) fn identity(&self) -> &Identity {
        self.identity
    }
    pub(super) fn revision(&self) -> &super::codex_session_slot::Revision {
        self.revision
    }
    pub(super) fn document(&self) -> &[u8] {
        self.document
    }
    pub(super) fn canonical_auth(&self) -> &Path {
        self.canonical_auth
    }
    pub(super) fn anchor(&mut self) -> &mut codex_enrollment::OwnerAnchorWriter<'a> {
        &mut self.anchor
    }
}

struct CanonicalGrant<'a> {
    binding: &'a GrantBinding,
    identity: &'a Identity,
    revision: &'a super::codex_session_slot::Revision,
    document: &'a [u8],
    canonical_auth: &'a Path,
    anchor: codex_enrollment::OwnerAnchorWriter<'a>,
}

/// App-owned settlement of an already authenticated durable owner journal.
/// This deliberately has no credential/document or delivery accessor and is
/// not accepted by Slot installation or the provider-read natural-use doors.
pub(super) struct MaintenanceGrant<'a> {
    canonical_auth: &'a Path,
    anchor: codex_enrollment::OwnerAnchorWriter<'a>,
    _scope: super::account_work::Scope,
}

impl std::fmt::Debug for MaintenanceGrant<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MaintenanceGrant(no-http, redacted)")
    }
}

impl<'a> MaintenanceGrant<'a> {
    pub(super) fn canonical_auth(&self) -> &Path {
        self.canonical_auth
    }
    pub(super) fn anchor(&mut self) -> &mut codex_enrollment::OwnerAnchorWriter<'a> {
        &mut self.anchor
    }
}

/// Cancellation prevents a consumer delivery; it does not revoke the app's
/// responsibility to settle an already durably retained provider response.
pub(super) fn with_maintenance_owner<R>(
    env: &Env,
    binding: &GrantBinding,
    scope: &super::account_work::Scope,
    maintain: impl FnOnce(MaintenanceGrant<'_>) -> Result<R, String>,
) -> Result<R, String> {
    let scope = scope.clone();
    with_canonical_owner(
        env,
        binding,
        |_| Ok(()),
        |grant| {
            maintain(MaintenanceGrant {
                canonical_auth: grant.canonical_auth,
                anchor: grant.anchor,
                _scope: scope,
            })
        },
    )
}

fn valid_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Private checkpoint evidence. This must never enter a public operation reply,
/// UI event, conversation, or diagnostic representation.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GrantReference {
    operation: String,
    capability: String,
    issuance_digest: String,
    producer_mac: String,
}

impl std::fmt::Debug for GrantReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GrantReference(redacted)")
    }
}

impl Drop for GrantReference {
    fn drop(&mut self) {
        self.capability.zeroize();
    }
}

impl GrantReference {
    pub(super) fn valid(&self) -> bool {
        valid_hex(&self.operation, 32)
            && valid_hex(&self.capability, 64)
            && valid_hex(&self.issuance_digest, 64)
            && valid_hex(&self.producer_mac, 64)
    }
}

/// Persisted correlation data, never admitted materialization authority. Every
/// use reopens canonical publication and authenticates the immutable issuer.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GrantBinding {
    pub(super) request: codex_enrollment::Request,
    pub(super) reference: GrantReference,
    pub(super) generation: u64,
}

impl std::fmt::Debug for GrantBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GrantBinding(redacted)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    NotDispatched,
    RejectedResponse,
    Prepared,
    Attempted,
    VerifiedResponse,
    UnverifiedResponse,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StagePredecessor {
    digest: Option<String>,
}
impl StagePredecessor {
    fn digest(&self) -> Option<&str> {
        self.digest.as_deref()
    }
    fn valid(&self) -> bool {
        self.digest().is_none_or(|digest| valid_hex(digest, 64))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    document: String,
    identity: Option<Identity>,
    login_owner: Option<String>,
    #[serde(default)]
    producer_seal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    predecessor: Option<StagePredecessor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    native_row: Option<codex_enrollment::Staging>,
}

impl Drop for Response {
    fn drop(&mut self) {
        self.document.zeroize();
        if let Some(owner) = &mut self.login_owner {
            owner.zeroize();
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RejectionEvidence {
    status: u16,
    body_digest: String,
    producer_mac: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retirement_seal: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    catalogue_scope: Option<catalogue::CatalogueScope>,
    request: codex_enrollment::Request,
    stage: PathBuf,
    stage_device: u64,
    stage_inode: u64,
    source_fence: String,
    capability: String,
    phase: Phase,
    response: Option<Response>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    no_dispatch_seal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rejection: Option<RejectionEvidence>,
}

impl Drop for Record {
    fn drop(&mut self) {
        self.capability.zeroize();
    }
}

impl Record {
    fn valid(&self) -> bool {
        self.catalogue_scope
            .as_ref()
            .is_none_or(catalogue::CatalogueScope::valid)
            && (self.schema != 7 || self.catalogue_scope.is_some())
            && (self.schema == 6
                || self.schema == 7
                || self
                    .response
                    .as_ref()
                    .is_none_or(|response| response.native_row.is_none()))
            && ((self.schema == 1
                && !matches!(self.phase, Phase::NotDispatched | Phase::RejectedResponse)
                && self.no_dispatch_seal.is_none()
                && self.rejection.is_none()
                && self
                    .response
                    .as_ref()
                    .is_none_or(|response| response.predecessor.is_none()))
                || (self.schema == 5
                    && self.phase == Phase::VerifiedResponse
                    && self.no_dispatch_seal.is_none()
                    && self.rejection.is_none()
                    && self
                        .response
                        .as_ref()
                        .and_then(|response| response.predecessor.as_ref())
                        .is_some_and(StagePredecessor::valid))
                || (matches!(self.schema, 6 | 7)
                    && self.phase == Phase::VerifiedResponse
                    && self.no_dispatch_seal.is_none()
                    && self.rejection.is_none()
                    && self.response.as_ref().is_some_and(|response| {
                        response
                            .predecessor
                            .as_ref()
                            .is_some_and(StagePredecessor::valid)
                            && response.native_row.as_ref().is_some_and(|staging| {
                                super::codex_native_row::staging_shape(staging)
                                    && staging.request == self.request
                                    && staging.directory.join("auth.json") == self.stage
                            })
                    }))
                || (self.schema == 7
                    && self.phase == Phase::VerifiedResponse
                    && self.catalogue_scope.is_some()
                    && self.no_dispatch_seal.is_none()
                    && self.rejection.is_none()
                    && self.response.as_ref().is_some_and(|response| {
                        response
                            .predecessor
                            .as_ref()
                            .is_some_and(StagePredecessor::valid)
                    }))
                || (self.schema == 2
                    && self.phase == Phase::NotDispatched
                    && self.rejection.is_none()
                    && self
                        .no_dispatch_seal
                        .as_ref()
                        .is_some_and(|seal| valid_hex(seal, 64)))
                || (matches!(self.schema, 3 | 4)
                    && self.phase == Phase::RejectedResponse
                    && self.no_dispatch_seal.is_none()
                    && self.rejection.as_ref().is_some_and(|evidence| {
                        ((self.schema == 3 && evidence.retirement_seal.is_none())
                            || (self.schema == 4
                                && evidence
                                    .retirement_seal
                                    .as_ref()
                                    .is_some_and(|seal| valid_hex(seal, 64))))
                            && evidence.status == 400
                            && valid_hex(&evidence.body_digest, 64)
                            && evidence
                                .producer_mac
                                .as_ref()
                                .is_some_and(|mac| valid_hex(mac, 64))
                    })))
            && self.request.valid()
            && valid_hex(&self.capability, 64)
            && valid_hex(&self.source_fence, 64)
            && stage_shape(&self.stage)
            && match (&self.phase, &self.response) {
                (Phase::Prepared | Phase::Attempted | Phase::NotDispatched, None) => true,
                (Phase::VerifiedResponse, Some(response)) => {
                    response.identity.as_ref().is_some_and(|identity| {
                        valid_hex(&identity.account, 64) && valid_hex(&identity.user, 64)
                    }) && response
                        .producer_seal
                        .as_ref()
                        .is_some_and(|seal| valid_hex(seal, 64))
                        && response.login_owner.as_ref().is_some_and(|owner| {
                            !owner.is_empty()
                                && owner.len() <= 256
                                && !owner.chars().any(char::is_control)
                        })
                        && decoded_document(response, MAX_DOCUMENT).is_ok()
                }
                (Phase::RejectedResponse, Some(response)) => {
                    response.identity.is_none()
                        && response.login_owner.is_none()
                        && response.producer_seal.is_none()
                        && response.predecessor.is_none()
                        && self.rejection.as_ref().is_some_and(|evidence| {
                            decoded_document(response, codex_app_login::MAX_REJECTION_BODY)
                                .ok()
                                .is_some_and(|body| {
                                    codex_app_login::is_definitive_code_rejection(
                                        evidence.status,
                                        body.as_slice(),
                                    ) && aterm_codec::hex::encode_lower(
                                        &aterm_digest::Sha256::digest(body.as_slice()),
                                    ) == evidence.body_digest
                                })
                        })
                }
                (Phase::UnverifiedResponse, Some(response)) => {
                    response.identity.is_none()
                        && response.login_owner.is_none()
                        && response.producer_seal.is_none()
                        && response.predecessor.is_none()
                        && decoded_document(response, MAX_RETAINED_DOCUMENT).is_ok()
                }
                _ => false,
            }
    }
}

fn decoded_document(response: &Response, max: usize) -> Result<SecretBytes, String> {
    if response.document.len() > max.saturating_add(2).saturating_div(3).saturating_mul(4) {
        return Err("retained OAuth response is too large".into());
    }
    let bytes = SecretBytes::new(
        aterm_codec::base64::decode_strict(response.document.as_bytes())
            .map_err(|_| "retained OAuth response is malformed")?,
    );
    if bytes.is_empty() || bytes.len() > max {
        return Err("retained OAuth response is too large".into());
    }
    Ok(bytes)
}

fn stage_path(path: &Path) -> Result<PathBuf, String> {
    if !stage_shape(path) {
        return Err("invalid staged OAuth response location".into());
    }
    let parent = path
        .parent()
        .ok_or("invalid staged OAuth response location")?;
    platform::check_private_dir(parent)
        .map_err(|_| "staged OAuth response directory is not private")?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|_| "staged OAuth response directory is unavailable")?;
    Ok(parent.join("auth.json"))
}

fn stage_shape(path: &Path) -> bool {
    path.is_absolute()
        && path.file_name().is_some_and(|name| name == "auth.json")
        && path.to_str().is_some_and(|raw| {
            raw.len() <= 4096
                && !raw.chars().any(char::is_control)
                && !raw.split('/').any(|part| matches!(part, "." | ".."))
        })
}

struct Store {
    catalogue_scope: Option<catalogue::CatalogueScope>,
    journal: platform::Store,
    directory: PathBuf,
    home: PathBuf,
    identity: age::Identity,
}

impl Store {
    fn historical_location(env: &Env, id: &str) -> Result<(PathBuf, PathBuf), String> {
        if !valid_hex(id, 32) {
            return Err("invalid grant operation".into());
        }
        let home = env.home.as_deref().ok_or("grant owner home unavailable")?;
        let root = env
            .accounts_store_for(super::accounts::Cli::Codex)
            .ok_or("grant owner store unavailable")?
            .dir
            .join("credential-owners");
        Ok((root.join(id), home.into()))
    }

    fn location(env: &Env, id: &str) -> Result<(PathBuf, PathBuf), String> {
        match catalogue::existing_scope(env, id)? {
            Some(scope) => Ok((
                catalogue::location(env, &scope)?,
                env.home.clone().ok_or("catalogue home unavailable")?,
            )),
            None => Self::historical_location(env, id),
        }
    }

    fn open(env: &Env, id: &str, prepare: bool) -> Result<Option<(Self, Option<Record>)>, String> {
        platform::require_available()?;
        let scope = catalogue::existing_scope(env, id)?;
        let (directory, home) = Self::location(env, id)?;
        let identity = platform::machine_identity(&home)
            .map_err(|_| "grant owner identity unavailable")?
            .ok_or("grant owner identity unavailable")?;
        let opened = if prepare {
            Some(platform::Store::open_owned(
                &directory,
                &home,
                MAX_ENVELOPE,
            )?)
        } else {
            platform::Store::open_owned_existing(&directory, &home, MAX_ENVELOPE)?
        };
        let Some((journal, plaintext)) = opened else {
            return Ok(None);
        };
        if journal.recipient() != identity.recipient() {
            return Err("grant owner identity changed".into());
        }
        let record = plaintext
            .map(|bytes| {
                let record: Record = aterm_json::from_slice(bytes.as_slice())
                    .map_err(|_| "grant custody record is invalid")?;
                if !record.valid() || record.request.id != id || record.catalogue_scope != scope {
                    return Err("grant custody record is invalid");
                }
                Ok(record)
            })
            .transpose()?;
        Ok(Some((
            Self {
                catalogue_scope: scope,
                journal,
                directory,
                home,
                identity,
            },
            record,
        )))
    }

    fn current(&self) -> Result<(), String> {
        let identity = platform::machine_identity(&self.home)
            .map_err(|_| "grant owner identity unavailable")?
            .ok_or("grant owner identity unavailable")?;
        if identity.recipient() != self.identity.recipient() {
            return Err("grant owner identity changed".into());
        }
        self.journal.unchanged().map_err(str::to_owned)
    }

    fn write(&mut self, record: &Record) -> Result<(), String> {
        if !record.valid() {
            return Err("grant custody record is invalid".into());
        }
        let plaintext = SecretBytes::new(
            aterm_json::to_vec(record).map_err(|_| "grant custody cannot be encoded")?,
        );
        if self.current().is_err() {
            let identity = platform::machine_identity(&self.home)
                .map_err(|_| "grant owner identity unavailable")?
                .ok_or("grant owner identity unavailable")?;
            if identity.recipient() != self.identity.recipient()
                || !self
                    .journal
                    .recover_exact_write(&identity, plaintext.as_slice())
                    .map_err(str::to_owned)?
            {
                return Err("grant custody changed".into());
            }
            self.current()?;
            return Ok(());
        }
        self.journal.save(plaintext.as_slice())?;
        aterm_durable::Flush::Durable
            .dir(&self.directory)
            .map_err(|_| "grant custody durability is unknown")?;
        self.current()
    }

    fn no_dispatch_mac(&self, record: &Record) -> Result<String, String> {
        if let Some(scope) = &record.catalogue_scope {
            if self.catalogue_scope.as_ref() != Some(scope) {
                return Err("global no-send scope changed".into());
            }
            let message = SecretBytes::new(
                aterm_json::to_vec(&(
                    "aterm/codex-code-not-dispatched/v2",
                    &record.request,
                    &record.stage,
                    record.stage_device,
                    record.stage_inode,
                    &record.source_fence,
                    &record.capability,
                    scope,
                ))
                .map_err(|_| "global no-send evidence unavailable")?,
            );
            return machine_mac(
                &self.identity,
                b"aterm/codex-code-not-dispatched/key/v2\0",
                message.as_slice(),
            );
        }
        let message = SecretBytes::new(
            aterm_json::to_vec(&(
                "aterm/codex-code-not-dispatched/v1",
                &record.request,
                &record.stage,
                record.stage_device,
                record.stage_inode,
                &record.source_fence,
                &record.capability,
            ))
            .map_err(|_| "no-dispatch evidence unavailable")?,
        );
        let salt = ring::hkdf::Salt::new(
            ring::hkdf::HKDF_SHA256,
            b"aterm/codex-grant-receipt-key/v1\0",
        );
        let derived = salt.extract(self.identity.secret_bytes());
        let info: &[&[u8]] = &[b"code-not-dispatched"];
        let key = derived
            .expand(info, ring::hkdf::HKDF_SHA256)
            .map_err(|_| "no-dispatch evidence unavailable")?;
        let mut bytes = [0u8; 32];
        key.fill(&mut bytes)
            .map_err(|_| "no-dispatch evidence unavailable")?;
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &bytes);
        bytes.zeroize();
        Ok(aterm_codec::hex::encode_lower(
            ring::hmac::sign(&key, message.as_slice()).as_ref(),
        ))
    }
    fn authenticates_no_dispatch(&self, record: &Record) -> bool {
        record.phase == Phase::NotDispatched
            && record.valid()
            && self
                .no_dispatch_mac(record)
                .ok()
                .zip(record.no_dispatch_seal.as_ref())
                .is_some_and(|(actual, expected)| {
                    aterm_digest::ct_eq(actual.as_bytes(), expected.as_bytes())
                })
    }

    fn rejection_mac(&self, record: &Record) -> Result<String, String> {
        let evidence = record
            .rejection
            .as_ref()
            .ok_or("provider rejection unavailable")?;
        let message = SecretBytes::new(
            aterm_json::to_vec(&(
                "aterm/codex-code-rejection/v1",
                &record.request,
                &record.stage,
                record.stage_device,
                record.stage_inode,
                &record.source_fence,
                &record.capability,
                evidence.status,
                &evidence.body_digest,
            ))
            .map_err(|_| "provider rejection unavailable")?,
        );
        let salt = ring::hkdf::Salt::new(
            ring::hkdf::HKDF_SHA256,
            b"aterm/codex-grant-receipt-key/v1\0",
        );
        let derived = salt.extract(self.identity.secret_bytes());
        let info: &[&[u8]] = &[b"code-rejection"];
        let key = derived
            .expand(info, ring::hkdf::HKDF_SHA256)
            .map_err(|_| "provider rejection unavailable")?;
        let mut bytes = [0u8; 32];
        key.fill(&mut bytes)
            .map_err(|_| "provider rejection unavailable")?;
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &bytes);
        bytes.zeroize();
        Ok(aterm_codec::hex::encode_lower(
            ring::hmac::sign(&key, message.as_slice()).as_ref(),
        ))
    }
    fn authenticates_rejection(&self, record: &Record) -> bool {
        record.phase == Phase::RejectedResponse
            && record.valid()
            && self
                .rejection_mac(record)
                .ok()
                .zip(
                    record
                        .rejection
                        .as_ref()
                        .and_then(|e| e.producer_mac.as_ref()),
                )
                .is_some_and(|(actual, expected)| {
                    aterm_digest::ct_eq(actual.as_bytes(), expected.as_bytes())
                })
    }

    fn rejection_retirement_mac(&self, record: &Record) -> Result<String, String> {
        let evidence = record
            .rejection
            .as_ref()
            .ok_or("rejection retirement unavailable")?;
        let producer = evidence
            .producer_mac
            .as_ref()
            .ok_or("rejection retirement unavailable")?;
        let message = SecretBytes::new(
            aterm_json::to_vec(&(
                "aterm/codex-code-rejection-retired/v1",
                &record.request,
                &record.stage,
                record.stage_device,
                record.stage_inode,
                &record.source_fence,
                &record.capability,
                evidence.status,
                &evidence.body_digest,
                producer,
            ))
            .map_err(|_| "rejection retirement unavailable")?,
        );
        let salt = ring::hkdf::Salt::new(
            ring::hkdf::HKDF_SHA256,
            b"aterm/codex-grant-receipt-key/v1\0",
        );
        let derived = salt.extract(self.identity.secret_bytes());
        let info: &[&[u8]] = &[b"code-rejection-retired"];
        let key = derived
            .expand(info, ring::hkdf::HKDF_SHA256)
            .map_err(|_| "rejection retirement unavailable")?;
        let mut bytes = [0u8; 32];
        key.fill(&mut bytes)
            .map_err(|_| "rejection retirement unavailable")?;
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &bytes);
        bytes.zeroize();
        Ok(aterm_codec::hex::encode_lower(
            ring::hmac::sign(&key, message.as_slice()).as_ref(),
        ))
    }
    fn authenticates_rejection_retirement(&self, record: &Record) -> bool {
        record.schema == 4
            && self.authenticates_rejection(record)
            && self
                .rejection_retirement_mac(record)
                .ok()
                .zip(
                    record
                        .rejection
                        .as_ref()
                        .and_then(|e| e.retirement_seal.as_ref()),
                )
                .is_some_and(|(actual, expected)| {
                    aterm_digest::ct_eq(actual.as_bytes(), expected.as_bytes())
                })
    }

    fn proof(&self, record: &Record, digest: &str) -> Result<String, String> {
        IssuerCryptography {
            identity: &self.identity,
            catalogue_scope: self.catalogue_scope.as_ref(),
        }
        .proof(record, digest)
    }

    fn issuance(&self, record: &Record) -> Result<GrantReference, String> {
        self.current()?;
        self.verify_origin(record)?;
        let digest = self
            .journal
            .sealed_digest()
            .ok_or("grant issuance is absent")?;
        let issuance_digest = aterm_codec::hex::encode_lower(&digest);
        let producer_mac = self.proof(record, &issuance_digest)?;
        Ok(GrantReference {
            operation: record.request.id.clone(),
            capability: record.capability.clone(),
            issuance_digest,
            producer_mac,
        })
    }
    fn origin_seal(&self, record: &Record) -> Result<String, String> {
        IssuerCryptography {
            identity: &self.identity,
            catalogue_scope: self.catalogue_scope.as_ref(),
        }
        .origin_seal(record)
    }

    fn verify_origin(&self, record: &Record) -> Result<(), String> {
        let saved = record
            .response
            .as_ref()
            .and_then(|response| response.producer_seal.as_deref())
            .ok_or("grant has no fresh producer seal")?;
        let expected = self.origin_seal(record)?;
        if !aterm_digest::ct_eq(saved.as_bytes(), expected.as_bytes()) {
            return Err("grant fresh producer seal differs".into());
        }
        Ok(())
    }
}

pub(super) fn machine_mac(
    identity: &age::Identity,
    domain: &[u8],
    message: &[u8],
) -> Result<String, String> {
    let salt = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, domain);
    let extracted = salt.extract(identity.secret_bytes());
    let info: &[&[u8]] = &[b"native-machine-producer"];
    let expanded = extracted
        .expand(info, ring::hkdf::HKDF_SHA256)
        .map_err(|_| "grant origin key unavailable")?;
    let mut bytes = [0u8; 32];
    expanded
        .fill(&mut bytes)
        .map_err(|_| "grant origin key unavailable")?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &bytes);
    bytes.zeroize();
    Ok(aterm_codec::hex::encode_lower(
        ring::hmac::sign(&key, message).as_ref(),
    ))
}

// A live preparation may release its owner claim while a browser waits. The
// original encrypted revision remains the only admissible reopening target.
struct PreparedStore {
    active: Option<Store>,
    parked: Option<(Env, [u8; 32])>,
}
impl PreparedStore {
    fn park(&mut self, env: &Env) -> Result<(), String> {
        let store = self.active.as_ref().ok_or("grant custody is not held")?;
        store.current()?;
        let digest = store
            .journal
            .sealed_digest()
            .ok_or("grant preparation is not durable")?;
        self.parked = Some((env.clone(), digest));
        self.active = None;
        Ok(())
    }
    fn resume(&mut self, operation: &str) -> Result<(), String> {
        if self.active.is_some() {
            return Ok(());
        }
        let (env, expected) = self.parked.as_ref().ok_or("grant custody is unavailable")?;
        let (store, record) =
            Store::open(env, operation, false)?.ok_or("grant preparation disappeared")?;
        if store.journal.sealed_digest() != Some(*expected)
            || !record.is_some_and(|record| record.phase == Phase::Prepared)
        {
            return Err("grant preparation changed while browser was active".into());
        }
        store.current()?;
        self.active = Some(store);
        self.parked = None;
        Ok(())
    }
    fn held(&self) -> Result<&Store, String> {
        self.active
            .as_ref()
            .ok_or_else(|| "grant custody is not held".into())
    }
    fn held_mut(&mut self) -> Result<&mut Store, String> {
        self.active
            .as_mut()
            .ok_or_else(|| "grant custody is not held".into())
    }
    fn write(&mut self, record: &Record) -> Result<(), String> {
        self.held_mut()?.write(record)
    }
    fn no_dispatch_mac(&self, record: &Record) -> Result<String, String> {
        self.held()?.no_dispatch_mac(record)
    }
    fn origin_seal(&self, record: &Record) -> Result<String, String> {
        self.held()?.origin_seal(record)
    }
    fn rejection_mac(&self, record: &Record) -> Result<String, String> {
        self.held()?.rejection_mac(record)
    }
    fn issuance(&self, record: &Record) -> Result<GrantReference, String> {
        self.held()?.issuance(record)
    }
    fn authenticates_no_dispatch(&self, record: &Record) -> bool {
        self.held()
            .is_ok_and(|store| store.authenticates_no_dispatch(record))
    }
    fn authenticates_rejection(&self, record: &Record) -> bool {
        self.held()
            .is_ok_and(|store| store.authenticates_rejection(record))
    }
}

// Only private issuer code computes producer MACs. Read-only observations can
// verify an existing occurrence, never mint a public proof from decoded data.
struct IssuerCryptography<'a> {
    identity: &'a age::Identity,
    catalogue_scope: Option<&'a catalogue::CatalogueScope>,
}
impl IssuerCryptography<'_> {
    fn proof(&self, record: &Record, digest: &str) -> Result<String, String> {
        let identity = record
            .response
            .as_ref()
            .and_then(|response| response.identity.as_ref())
            .ok_or("unverified response has no producer receipt")?;
        let message = SecretBytes::new(
            aterm_json::to_vec(&(
                "aterm/codex-fresh-code-issuance/v1",
                &record.request,
                &record.stage,
                &record.source_fence,
                record.stage_device,
                record.stage_inode,
                &record.capability,
                identity,
                digest,
            ))
            .map_err(|_| "grant issuance cannot be encoded")?,
        );
        let salt = ring::hkdf::Salt::new(
            ring::hkdf::HKDF_SHA256,
            b"aterm/codex-grant-receipt-key/v1\0",
        );
        let derived = salt.extract(self.identity.secret_bytes());
        let info: &[&[u8]] = &[b"fresh-code-producer"];
        let key = derived
            .expand(info, ring::hkdf::HKDF_SHA256)
            .map_err(|_| "grant issuance key unavailable")?;
        let mut bytes = [0u8; 32];
        key.fill(&mut bytes)
            .map_err(|_| "grant issuance key unavailable")?;
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &bytes);
        bytes.zeroize();
        Ok(aterm_codec::hex::encode_lower(
            ring::hmac::sign(&key, message.as_slice()).as_ref(),
        ))
    }

    fn origin_seal(&self, record: &Record) -> Result<String, String> {
        let response = record.response.as_ref().ok_or("grant response absent")?;
        let identity = response
            .identity
            .as_ref()
            .ok_or("unverified grant response")?;
        let owner = response
            .login_owner
            .as_deref()
            .ok_or("unverified grant response")?;
        let legacy = (
            &record.request,
            &record.stage,
            record.stage_device,
            record.stage_inode,
            &record.source_fence,
            &record.capability,
            identity,
            owner,
            &response.document,
        );
        if let Some(scope) = &record.catalogue_scope {
            if record.schema != 7 || self.catalogue_scope != Some(scope) || !scope.valid() {
                return Err("global issuer scope changed".into());
            }
            let body = SecretBytes::new(
                aterm_json::to_vec(&(
                    "aterm/codex-code-response-origin/v4",
                    legacy,
                    &response.predecessor,
                    &response.native_row,
                    scope,
                ))
                .map_err(|_| "global origin cannot be encoded")?,
            );
            return machine_mac(
                self.identity,
                b"aterm/codex-code-response-origin/key/v4\0",
                body.as_slice(),
            );
        }
        if let Some(staging) = &response.native_row {
            if record.schema != 6 || !super::codex_native_row::staging_shape(staging) {
                return Err("native row issuer evidence unavailable".into());
            }
            let predecessor = response
                .predecessor
                .as_ref()
                .ok_or("native row predecessor unavailable")?;
            let body = SecretBytes::new(
                aterm_json::to_vec(&(
                    "aterm/codex-code-response-origin/v3",
                    legacy,
                    predecessor,
                    staging,
                ))
                .map_err(|_| "native row origin cannot be encoded")?,
            );
            machine_mac(
                self.identity,
                b"aterm/codex-code-response-origin/key/v3\0",
                body.as_slice(),
            )
        } else if let Some(predecessor) = &response.predecessor {
            let body = SecretBytes::new(
                aterm_json::to_vec(&("aterm/codex-code-response-origin/v2", legacy, predecessor))
                    .map_err(|_| "grant origin cannot be encoded")?,
            );
            machine_mac(
                self.identity,
                b"aterm/codex-code-response-origin/key/v2\0",
                body.as_slice(),
            )
        } else {
            let body = SecretBytes::new(
                aterm_json::to_vec(&(
                    "aterm/codex-code-response-origin/v1",
                    &record.request,
                    &record.stage,
                    record.stage_device,
                    record.stage_inode,
                    &record.source_fence,
                    &record.capability,
                    identity,
                    owner,
                    &response.document,
                ))
                .map_err(|_| "grant origin cannot be encoded")?,
            );
            machine_mac(
                self.identity,
                b"aterm/codex-code-response-origin/key/v1\0",
                body.as_slice(),
            )
        }
    }
}

#[path = "codex_grant_issuer_observation.rs"]
mod issuer_observation;

/// One staged exchange's durable custody, prepared before the authorization code
/// can be spent. Different enrollments hold different locks.
pub(super) struct PreparedGrant {
    env: Env,
    store: PreparedStore,
    record: Record,
    stage_witness: codex_app_login::StagedWitness,
    fresh_origin: bool,
    no_dispatch_pending: bool,
    rejection_origin: bool,
    native_scope: Option<std::sync::Arc<super::native_enrollment_admission::NativeBrowserTurn>>,
    row_admission: Option<super::codex_native_row::NativeRowAdmission>,
}

impl std::fmt::Debug for PreparedGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedGrant(redacted)")
    }
}

pub(super) fn prepare(
    env: &Env,
    request: &codex_enrollment::Request,
    stage: &Path,
    source_fence: &str,
) -> Result<PreparedGrant, String> {
    platform::require_available()?;
    if !request.valid() || !valid_hex(source_fence, 64) {
        return Err("invalid grant preparation".into());
    }
    let stage = stage_path(stage)?;
    let stage_witness = codex_app_login::StagedWitness::open(&stage)?;
    let (_, stage_device, stage_inode) = stage_witness.coordinates()?;
    let catalogue_scope = catalogue::prepare(env, request, &stage)?;
    let (directory, _) = Store::location(env, &request.id)?;
    let root = directory
        .parent()
        .ok_or("grant custody directory unavailable")?;
    platform::ensure_private_dir(root).map_err(|_| "grant custody directory unavailable")?;
    let _registry = platform::registry_lock(root).map_err(|_| "grant registry is busy")?;
    let mut entries = std::fs::read_dir(root).map_err(|_| "grant registry unavailable")?;
    let count = entries.by_ref().take(MAX_PENDING + 2).count();
    if count > MAX_PENDING && !directory.exists() {
        return Err("grant custody is full".into());
    }
    let (mut store, existing) =
        Store::open(env, &request.id, true)?.ok_or("grant custody unavailable")?;
    let record = if let Some(record) = existing {
        if record.request != *request
            || record.stage != stage
            || record.stage_device != stage_device
            || record.stage_inode != stage_inode
            || record.source_fence != source_fence
            || record.phase != Phase::Prepared
        {
            return Err("grant exchange is already retained or uncertain".into());
        }
        record
    } else {
        let mut capability = [0u8; 32];
        aterm_uds::rand::fill(&mut capability).map_err(|_| "grant nonce unavailable")?;
        let capability_text = aterm_codec::hex::encode_lower(&capability);
        capability.zeroize();
        let record = Record {
            schema: 1,
            catalogue_scope: catalogue_scope.clone(),
            request: request.clone(),
            stage,
            stage_device,
            stage_inode,
            source_fence: source_fence.to_owned(),
            capability: capability_text,
            phase: Phase::Prepared,
            response: None,
            no_dispatch_seal: None,
            rejection: None,
        };
        store.write(&record)?;
        record
    };
    Ok(PreparedGrant {
        env: env.clone(),
        store: PreparedStore {
            active: Some(store),
            parked: None,
        },
        record,
        stage_witness,
        fresh_origin: false,
        no_dispatch_pending: false,
        rejection_origin: false,
        native_scope: None,
        row_admission: None,
    })
}

/// Resource identity derived only from an authenticated retained native response.
/// Labels, email, profile paths and locally minted owner IDs are excluded.
#[derive(PartialEq, Eq)]
pub(super) struct PublicationIdentity {
    operation: String,
    resources: Vec<String>,
}
impl PublicationIdentity {
    pub(super) fn operation(&self) -> &str {
        &self.operation
    }
    pub(super) fn resources(&self) -> &[String] {
        &self.resources
    }
}
fn response_resource_identity(
    request: &codex_enrollment::Request,
    identity: &Identity,
    document: &[u8],
) -> Result<PublicationIdentity, String> {
    let document: aterm_json::Value =
        aterm_json::from_slice(document).map_err(|_| "native publication identity unavailable")?;
    let refresh = document
        .get("tokens")
        .and_then(|tokens| tokens.get("refresh_token"))
        .and_then(aterm_json::Value::as_str)
        .filter(|refresh| !refresh.is_empty())
        .ok_or("native publication refresh lineage unavailable")?;
    let resource = |domain: &[u8], bytes: &[u8]| {
        let mut input = zeroize::Zeroizing::new(domain.to_vec());
        input.extend_from_slice(bytes);
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(input.as_slice()))
    };
    let mut resources = vec![
        resource(
            b"aterm native publication account v1\0",
            identity.account.as_bytes(),
        ),
        resource(
            b"aterm native publication user v1\0",
            identity.user.as_bytes(),
        ),
        resource(b"aterm native publication refresh v1\0", refresh.as_bytes()),
    ];
    resources.sort();
    resources.dedup();
    Ok(PublicationIdentity {
        operation: request.id.clone(),
        resources,
    })
}
fn publication_identity(
    env: &Env,
    request: &codex_enrollment::Request,
    grant: &RetainedGrant,
) -> Result<PublicationIdentity, String> {
    with_retained_response(env, request, grant, |identity, document, _, _, _| {
        response_resource_identity(request, identity, document)
    })
}

/// Historical authenticated resource names, never publication or source proof.
#[derive(PartialEq, Eq)]
pub(super) struct RetirementIdentity(PublicationIdentity);
impl RetirementIdentity {
    pub(super) fn operation(&self) -> &str {
        self.0.operation()
    }
    pub(super) fn resources(&self) -> &[String] {
        self.0.resources()
    }
}
fn retirement_identity(
    env: &Env,
    candidate: &super::codex_native_retirement::NativeRetirementCandidate,
    grant: &RetainedGrant,
) -> Result<RetirementIdentity, String> {
    let (owner, record) = open_authenticated(env, candidate.request(), &grant.reference())?;
    let response = record
        .response
        .as_ref()
        .ok_or("native retirement response unavailable")?;
    if !matches!(record.schema, 6 | 7) || response.native_row.as_ref() != Some(candidate.staging())
    {
        return Err("native retirement issuer stage changed".into());
    }
    let identity = response
        .identity
        .as_ref()
        .ok_or("native retirement identity unavailable")?;
    let document = decoded_document(response, MAX_DOCUMENT)?;
    let result = response_resource_identity(candidate.request(), identity, document.as_slice())?;
    owner.current()?;
    Ok(RetirementIdentity(result))
}
/// Wait only after the actual response is durable. The first identity read
/// drops owner/Auth claims before higher-ranked publication resources wait;
/// successful admission then reopens and checks the same immutable response.
pub(super) fn admit_native_publication_within(
    env: &Env,
    request: &codex_enrollment::Request,
    grant: &RetainedGrant,
    turn: &super::native_enrollment_admission::NativeBrowserTurn,
    scope: &super::account_work::Scope,
    deadline: super::wake_deadline::WakeDeadline,
) -> Result<(), String> {
    let identity = publication_identity(env, request, grant)?;
    turn.admit_publication_within(&identity, scope, deadline)?;
    let result = publication_identity(env, request, grant).and_then(|current| {
        if current != identity || scope.cancelled() || deadline.passed() {
            return Err("native publication identity or work changed".into());
        }
        turn.check_publication()
    });
    if result.is_err() {
        turn.reject_publication();
    }
    result
}

/// Retained fresh-code evidence. It is still pending canonical account
/// publication and sole-lineage/consumer custody admission.
pub(super) struct RetainedGrant {
    reference: GrantReference,
    request: codex_enrollment::Request,
}

impl std::fmt::Debug for RetainedGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RetainedGrant(redacted)")
    }
}

impl RetainedGrant {
    pub(super) fn reference(&self) -> GrantReference {
        self.reference.clone()
    }

    pub(super) fn matches_request(&self, request: &codex_enrollment::Request) -> bool {
        self.request == *request
    }
}

pub(super) struct RetainedResponse {
    _operation: String,
    _issuance_digest: String,
}

impl std::fmt::Debug for RetainedResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RetainedResponse(unverified, redacted)")
    }
}

/// A failed write still owns the completed response and custody claim. The
/// operation runtime must retain its Scope until retry returns a settled result.
#[must_use = "completed OAuth response custody must remain owned until encrypted settlement"]
pub(super) struct RetentionFailure {
    pending: PreparedGrant,
}

impl std::fmt::Debug for RetentionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RetentionFailure(grant custody retained, redacted)")
    }
}

#[must_use = "retain Pending with the operation Scope; only Verified permits publication"]
pub(super) enum CaptureOutcome {
    Verified(RetainedGrant),
    NotDispatched(NotDispatched),
    Rejected(RejectedCompletion),
    Unverified(RetainedResponse),
    Pending(Box<RetentionFailure>),
}

impl std::fmt::Debug for CaptureOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(_) => f.write_str("CaptureOutcome(Rejected)"),
            Self::NotDispatched(_) => f.write_str("CaptureOutcome(NotDispatched)"),
            Self::Verified(receipt) => f.debug_tuple("CaptureOutcome").field(receipt).finish(),
            Self::Unverified(receipt) => f.debug_tuple("CaptureOutcome").field(receipt).finish(),
            Self::Pending(pending) => f.debug_tuple("CaptureOutcome").field(pending).finish(),
        }
    }
}

impl RetentionFailure {
    pub(super) fn retry(self: Box<Self>) -> CaptureOutcome {
        let Self { pending } = *self;
        if pending.no_dispatch_pending {
            pending.settle_not_dispatched()
        } else {
            pending.settle_response()
        }
    }
}

/// Durable provider rejection, not credential, no-send, or retirement authority.
pub(super) struct RejectedCompletion {
    _request: codex_enrollment::Request,
}
impl RejectedCompletion {
    #[cfg(all(test, unix))]
    pub(super) fn matches_request(&self, request: &codex_enrollment::Request) -> bool {
        self._request == *request
    }
}

/// Combined persisted facts: an authentic provider rejection and actual
/// request-bound owned-work retirement. No grant or cancellation authority.
pub(super) struct RetiredRejection {
    request: codex_enrollment::Request,
}
impl RetiredRejection {
    pub(super) fn request(&self) -> &codex_enrollment::Request {
        &self.request
    }
}

/// Authenticated proof of the producer's actual pre-send branch. This grants
/// no credential or provider authority and cannot be reconstructed from Attempted.
pub(super) struct NotDispatched {
    request: codex_enrollment::Request,
}
impl NotDispatched {
    pub(super) fn matches_request(&self, request: &codex_enrollment::Request) -> bool {
        self.request == *request
    }
}

/// Only a successfully persisted exact live completion can release exchange
/// exclusion. This grants neither provider identity nor publication authority.
pub(super) struct DurableExchangeSettlement {
    operation: String,
}
impl DurableExchangeSettlement {
    pub(super) fn operation(&self) -> &str {
        &self.operation
    }
}

impl PreparedGrant {
    pub(super) fn with_row_admission(
        mut self,
        admission: super::codex_native_row::NativeRowAdmission,
    ) -> Result<Self, String> {
        let staging = admission.staging();
        if !super::codex_native_row::staging_shape(staging)
            || staging.request != self.record.request
            || staging.directory.join("auth.json") != self.record.stage
            || !self.stage_witness.matches_auth_file(&self.record.stage)
            || self
                .native_scope
                .as_ref()
                .is_none_or(|scope| !scope.matches_request(&self.record.request))
        {
            return Err("native row admission does not match this producer".into());
        }
        self.row_admission = Some(admission);
        Ok(self)
    }

    pub(super) fn with_native_scope(
        mut self,
        env: &Env,
        scope: Option<std::sync::Arc<super::native_enrollment_admission::NativeBrowserTurn>>,
    ) -> Result<Self, String> {
        if let Some(scope) = &scope {
            if !scope.matches_request(&self.record.request) {
                return Err("native browser reservation belongs to another enrollment".into());
            }
            scope.check_preparation(
                &env.accounts_store_for(super::accounts::Cli::Codex)
                    .ok_or("native account store unavailable")?,
            )?;
            scope.bind_source_home(
                env.home
                    .as_deref()
                    .ok_or("native source home unavailable")?,
            )?;
            self.store.park(env)?;
        }
        self.native_scope = scope;
        Ok(self)
    }
    pub(super) fn native_scope(
        &self,
    ) -> Option<std::sync::Arc<super::native_enrollment_admission::NativeBrowserTurn>> {
        self.native_scope.clone()
    }

    fn catalogue_response_durable(&self) -> Result<(), String> {
        if let Some(scope) = &self.record.catalogue_scope {
            let store = self
                .store
                .active
                .as_ref()
                .ok_or("catalogue issuer custody unavailable")?;
            catalogue::received(&self.env, scope, store, &self.record)?;
        }
        Ok(())
    }
    fn release_settled_exchange(&self) {
        if let Some(scope) = &self.native_scope {
            scope.response_durable(&DurableExchangeSettlement {
                operation: self.record.request.id.clone(),
            });
        }
    }
    fn settle_not_dispatched(mut self) -> CaptureOutcome {
        self.no_dispatch_pending = true;
        if self.store.resume(&self.record.request.id).is_err() {
            return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
        }
        if matches!(self.record.phase, Phase::Prepared | Phase::Attempted) {
            // First settle an uncertain acknowledgment of the exact attempted
            // write. Only the live no-send branch may advance it to this proof.
            if self.store.write(&self.record).is_err() {
                return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
            }
            let Ok(seal) = self.store.no_dispatch_mac(&self.record) else {
                return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
            };
            self.record.phase = Phase::NotDispatched;
            self.record.schema = 2;
            self.record.no_dispatch_seal = Some(seal);
        }
        if self.record.phase != Phase::NotDispatched
            || self.store.write(&self.record).is_err()
            || !self.store.authenticates_no_dispatch(&self.record)
        {
            return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
        }
        if self.catalogue_response_durable().is_err() {
            return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
        }
        self.release_settled_exchange();
        CaptureOutcome::NotDispatched(NotDispatched {
            request: self.record.request.clone(),
        })
    }

    pub(super) fn begin_exchange(&mut self) -> Result<(), String> {
        self.store.resume(&self.record.request.id)?;
        if self.record.phase != Phase::Prepared {
            return Err("grant exchange is already attempted".into());
        }
        if let Some(scope) = &self.record.catalogue_scope {
            catalogue::attempted(&self.env, scope)?;
        }
        self.record.phase = Phase::Attempted;
        self.store.write(&self.record)
    }

    pub(super) fn retain_verified(
        mut self,
        fresh: codex_app_login::FreshCodeGrant<'_>,
    ) -> CaptureOutcome {
        let predecessor = StagePredecessor {
            digest: fresh.predecessor().map(str::to_owned),
        };
        fresh.consume(|identity, owner, document, witness| {
            // A completed but unsupported response still gets encrypted custody;
            // it cannot acquire verified grant or stage publication authority.
            let verified = self.record.phase == Phase::Attempted
                && witness.same_open_parent(&self.stage_witness)
                && self.stage_witness.matches_auth_file(&self.record.stage)
                && self
                    .record
                    .stage
                    .parent()
                    .is_some_and(|parent| platform::check_private_dir(parent).is_ok())
                && witness.matches_auth_file(&self.record.stage)
                && document.len() <= MAX_DOCUMENT;
            self.capture_response(
                document,
                verified.then(|| identity.clone()),
                verified.then(|| owner.to_owned()),
            );
            if verified {
                self.record.schema = 5;
                self.record
                    .response
                    .as_mut()
                    .expect("captured response")
                    .predecessor = Some(predecessor);
            }
            if self.record.phase == Phase::VerifiedResponse
                && let Some(admission) = self.row_admission.as_ref()
            {
                self.record.schema = 6;
                if let Some(response) = self.record.response.as_mut() {
                    response.native_row = Some(admission.staging().clone());
                }
            }
            self.settle_response()
        })
    }

    fn retain_rejected(mut self, rejection: codex_app_login::CodeRejection<'_>) -> CaptureOutcome {
        rejection.consume(|status, body, witness| {
            if !witness.same_open_parent(&self.stage_witness)
                || !witness.matches_auth_file(&self.record.stage)
            {
                return self.retain_unverified(body);
            }
            self.capture_rejection(status, body);
            self.settle_response()
        })
    }

    fn capture_rejection(&mut self, status: u16, body: &[u8]) {
        self.capture_response(body, None, None);
        self.record.schema = 3;
        self.record.phase = Phase::RejectedResponse;
        self.record.rejection = Some(RejectionEvidence {
            status,
            body_digest: aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(body)),
            producer_mac: None,
            retirement_seal: None,
        });
        // Production reaches this only through the private live exchange proof.
        self.rejection_origin = true;
    }

    pub(super) fn retain_unverified(mut self, document: &[u8]) -> CaptureOutcome {
        self.capture_response(document, None, None);
        self.settle_response()
    }

    fn capture_response(
        &mut self,
        document: &[u8],
        identity: Option<Identity>,
        owner: Option<String>,
    ) {
        // The actual producer bounds its HTTP response before making this call.
        // The encoded response stays owned even if a later durability check fails.
        let encoded = aterm_codec::base64::encode(document)
            .expect("a native bounded OAuth response fits base64's allocation bound");
        self.record.phase = if identity.is_some() {
            Phase::VerifiedResponse
        } else {
            Phase::UnverifiedResponse
        };
        self.record.response = Some(Response {
            document: encoded,
            identity,
            login_owner: owner,
            producer_seal: None,
            predecessor: None,
            native_row: None,
        });
        // Only the live FreshCodeGrant callback supplies a verified identity
        // in production. Encrypted recovery never constructs this volatile fact.
        self.fresh_origin = self.record.phase == Phase::VerifiedResponse;
    }

    fn settle_response(mut self) -> CaptureOutcome {
        if self.record.phase == Phase::VerifiedResponse && self.record.catalogue_scope.is_some() {
            self.record.schema = 7;
        }
        if self.record.phase == Phase::RejectedResponse
            && self
                .record
                .rejection
                .as_ref()
                .is_some_and(|evidence| evidence.producer_mac.is_none())
        {
            if !self.rejection_origin {
                return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
            }
            let Ok(mac) = self.store.rejection_mac(&self.record) else {
                return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
            };
            self.record
                .rejection
                .as_mut()
                .expect("rejection exists")
                .producer_mac = Some(mac);
        }

        if self.record.phase == Phase::VerifiedResponse
            && self
                .record
                .response
                .as_ref()
                .is_some_and(|response| response.producer_seal.is_none())
        {
            if !self.fresh_origin {
                return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
            }
            let Ok(seal) = self.store.origin_seal(&self.record) else {
                return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
            };
            self.record
                .response
                .as_mut()
                .expect("verified response exists")
                .producer_seal = Some(seal);
        }
        if self.store.write(&self.record).is_err() {
            return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
        }
        if self.catalogue_response_durable().is_err() {
            return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
        }
        if self.record.phase == Phase::RejectedResponse {
            if !self.store.authenticates_rejection(&self.record) {
                return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
            }
            self.release_settled_exchange();
            CaptureOutcome::Rejected(RejectedCompletion {
                _request: self.record.request.clone(),
            })
        } else if self.record.phase == Phase::VerifiedResponse {
            match self.store.issuance(&self.record) {
                Ok(reference) => {
                    self.release_settled_exchange();
                    CaptureOutcome::Verified(RetainedGrant {
                        reference,
                        request: self.record.request.clone(),
                    })
                }
                Err(_) => CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self })),
            }
        } else {
            let Some(digest) = self
                .store
                .held()
                .ok()
                .and_then(|store| store.journal.sealed_digest())
            else {
                return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
            };
            self.release_settled_exchange();
            CaptureOutcome::Unverified(RetainedResponse {
                _operation: self.record.request.id.clone(),
                _issuance_digest: aterm_codec::hex::encode_lower(&digest),
            })
        }
    }
}

impl codex_app_login::CodeGrantCapture for PreparedGrant {
    type Output = CaptureOutcome;

    fn claim_exchange(&mut self) -> Result<(), String> {
        if !self.stage_witness.matches_auth_file(&self.record.stage) {
            return Err("grant preparation stage changed".into());
        }
        self.store.resume(&self.record.request.id)
    }

    fn begin_exchange(&mut self) -> Result<(), String> {
        PreparedGrant::begin_exchange(self)
    }

    fn not_dispatched(self) -> Self::Output {
        self.settle_not_dispatched()
    }

    fn retain_verified(self, grant: codex_app_login::FreshCodeGrant<'_>) -> Self::Output {
        PreparedGrant::retain_verified(self, grant)
    }

    fn retain_unverified(self, document: &[u8]) -> Self::Output {
        PreparedGrant::retain_unverified(self, document)
    }

    fn retain_rejected(self, rejection: codex_app_login::CodeRejection<'_>) -> Self::Output {
        PreparedGrant::retain_rejected(self, rejection)
    }

    fn verified_durable(output: &Self::Output) -> bool {
        matches!(output, CaptureOutcome::Verified(_))
    }
}

/// Recover only authenticated encrypted fresh-code evidence. A surviving plain
/// stage document or copied login_owner_id is never a recovery constructor.
pub(super) fn recover(
    env: &Env,
    request: &codex_enrollment::Request,
    reference: &GrantReference,
) -> Result<RetainedGrant, String> {
    let (_store, _record) = open_authenticated(env, request, reference)?;
    Ok(RetainedGrant {
        reference: reference.clone(),
        request: request.clone(),
    })
}

/// Validate both signed facts and settle their exact encrypted payload.
fn finish_rejection_retirement(
    store: &mut Store,
    record: &Record,
) -> Result<RetiredRejection, String> {
    if !store.authenticates_rejection_retirement(record) {
        return Err("rejection retirement unavailable".into());
    }
    store.current()?;
    let plaintext = SecretBytes::new(
        aterm_json::to_vec(record).map_err(|_| "rejection retirement unavailable")?,
    );
    if !store
        .journal
        .recover_exact_write(&store.identity, plaintext.as_slice())
        .map_err(str::to_owned)?
    {
        return Err("rejection retirement changed".into());
    }
    store.current()?;
    Ok(RetiredRejection {
        request: record.request.clone(),
    })
}

/// Persist an already observed real Scope retirement before the canonical
/// terminal write. Public recipient encryption cannot mint this second fact.
pub(super) fn retain_rejection_retirement(
    env: &Env,
    retired: &codex_enrollment::Retired,
) -> Result<Option<RetiredRejection>, String> {
    let request = retired.request();
    let (directory, _) = Store::location(env, &request.id)?;
    match std::fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("rejection retirement unavailable".into()),
        Ok(_) => {}
    }

    let Some((mut store, record)) = Store::open(env, &request.id, false)? else {
        return Ok(None);
    };
    let Some(mut record) = record else {
        return Ok(None);
    };
    if record.request != *request {
        return Err("rejection retirement request changed".into());
    }
    if record.phase != Phase::RejectedResponse {
        return Ok(None);
    }
    if !store.authenticates_rejection(&record) {
        return Err("provider rejection unavailable".into());
    }
    if record.schema == 3 {
        let seal = store.rejection_retirement_mac(&record)?;
        record.schema = 4;
        record
            .rejection
            .as_mut()
            .expect("authenticated rejection")
            .retirement_seal = Some(seal);
        store.write(&record)?;
    }
    finish_rejection_retirement(&mut store, &record).map(Some)
}

pub(super) fn recover_rejection_retirement(
    env: &Env,
    request: &codex_enrollment::Request,
) -> Result<Option<RetiredRejection>, String> {
    if !request.valid() {
        return Err("invalid rejection retirement request".into());
    }
    let (directory, _) = Store::location(env, &request.id)?;
    match std::fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("rejection retirement unavailable".into()),
        Ok(_) => {}
    }
    let Some((mut store, record)) = Store::open(env, &request.id, false)? else {
        return Ok(None);
    };
    let Some(record) = record else {
        return Ok(None);
    };
    if record.request != *request {
        return Err("rejection retirement request changed".into());
    }
    if record.phase != Phase::RejectedResponse || record.schema != 4 {
        return Ok(None);
    }
    finish_rejection_retirement(&mut store, &record).map(Some)
}

#[cfg(all(test, unix))]
pub(super) fn recover_rejection(
    env: &Env,
    request: &codex_enrollment::Request,
) -> Result<Option<RejectedCompletion>, String> {
    if !request.valid() {
        return Err("invalid provider rejection request".into());
    }
    let (directory, _) = Store::location(env, &request.id)?;
    match std::fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("provider rejection unavailable".into()),
        Ok(_) => {}
    }
    let Some((mut store, record)) = Store::open(env, &request.id, false)? else {
        return Ok(None);
    };
    let Some(record) = record else {
        return Ok(None);
    };
    if record.request != *request {
        return Err("provider rejection request changed".into());
    }
    if record.phase != Phase::RejectedResponse {
        return Ok(None);
    }
    if !store.authenticates_rejection(&record) {
        return Err("provider rejection unavailable".into());
    }
    store.current()?;
    let plaintext = SecretBytes::new(
        aterm_json::to_vec(&record).map_err(|_| "provider rejection unavailable")?,
    );
    if !store
        .journal
        .recover_exact_write(&store.identity, plaintext.as_slice())
        .map_err(str::to_owned)?
    {
        return Err("provider rejection changed".into());
    }
    store.current()?;
    Ok(Some(RejectedCompletion {
        _request: request.clone(),
    }))
}

pub(super) fn recover_not_dispatched(
    env: &Env,
    request: &codex_enrollment::Request,
) -> Result<Option<NotDispatched>, String> {
    if !request.valid() {
        return Err("invalid no-dispatch request".into());
    }
    let (directory, _) = Store::location(env, &request.id)?;
    match std::fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("grant custody unavailable".into()),
        Ok(_) => {}
    }
    let Some((mut store, record)) = Store::open(env, &request.id, false)? else {
        return Ok(None);
    };
    let Some(record) = record else {
        return Ok(None);
    };
    if record.request != *request {
        return Err("grant request changed".into());
    }
    if record.phase != Phase::NotDispatched {
        return Ok(None);
    }
    if !store.authenticates_no_dispatch(&record) {
        return Err("no-dispatch evidence unavailable".into());
    }
    store.current()?;
    // A prior writer can have observed a failed fsync after rename. A cold
    // Signal worker must settle the exact authenticated payload durably before
    // minting terminal evidence; reading a valid ciphertext alone is insufficient.
    let plaintext = SecretBytes::new(
        aterm_json::to_vec(&record).map_err(|_| "no-dispatch evidence unavailable")?,
    );
    if !store
        .journal
        .recover_exact_write(&store.identity, plaintext.as_slice())
        .map_err(str::to_owned)?
    {
        return Err("no-dispatch evidence changed".into());
    }
    store.current()?;
    Ok(Some(NotDispatched {
        request: request.clone(),
    }))
}

/// Recover a completed exchange whose durable response preceded checkpoint
/// attachment. The inner seal was minted by the live exchange producer; public
/// recipient encryption and surviving stage bytes cannot supply that authority.
pub(super) fn recover_pending(
    env: &Env,
    request: &codex_enrollment::Request,
) -> Result<RetainedGrant, String> {
    if !request.valid() {
        return Err("invalid retained grant request".into());
    }
    let (store, record) =
        Store::open(env, &request.id, false)?.ok_or("retained grant is absent")?;
    let record = record.ok_or("retained grant is absent")?;
    if record.request != *request || record.phase != Phase::VerifiedResponse {
        return Err("retained grant does not match this enrollment".into());
    }
    // issuance checks the producer seal before creating the outer immutable
    // ciphertext receipt. It never repairs or invents a missing inner seal.
    let reference = store.issuance(&record)?;
    Ok(RetainedGrant {
        reference,
        request: request.clone(),
    })
}

fn open_authenticated(
    env: &Env,
    request: &codex_enrollment::Request,
    reference: &GrantReference,
) -> Result<(Store, Record), String> {
    if !request.valid() || !reference.valid() || reference.operation != request.id {
        return Err("invalid retained grant reference".into());
    }
    let (store, record) =
        Store::open(env, &request.id, false)?.ok_or("retained grant is absent")?;
    let record = record.ok_or("retained grant is absent")?;
    let actual_digest = store
        .journal
        .sealed_digest()
        .ok_or("retained grant is absent")?;
    store.verify_origin(&record)?;
    let expected_mac = store.proof(&record, &reference.issuance_digest)?;
    if record.request != *request
        || record.phase != Phase::VerifiedResponse
        || !aterm_digest::ct_eq(
            reference.capability.as_bytes(),
            record.capability.as_bytes(),
        )
        || !aterm_digest::ct_eq(
            reference.issuance_digest.as_bytes(),
            aterm_codec::hex::encode_lower(&actual_digest).as_bytes(),
        )
        || !aterm_digest::ct_eq(reference.producer_mac.as_bytes(), expected_mac.as_bytes())
    {
        return Err("retained grant does not match this enrollment".into());
    }
    Ok((store, record))
}

/// Authenticated immutable issuer, retained under its actual owner claim. It
/// can sign only the exact staging/reference already authenticated on open.
pub(super) struct RowPublicationIssuer {
    owner: Store,
    record: Record,
    reference: GrantReference,
}

/// Exact producer bytes under the original issuer and stage authentication
/// claims. This cannot be reconstructed from a row receipt or document bytes.
pub(super) struct PublicationResponse<'a> {
    issuer: &'a RowPublicationIssuer,
    identity: Identity,
    document: SecretBytes,
    witness: codex_app_login::StagedWitness,
    auth: codex_app_login::AuthLock,
}
impl PublicationResponse<'_> {
    fn current(&self) -> Result<(), String> {
        self.issuer.owner.current()?;
        let record = &self.issuer.record;
        let (path, device, inode) = self.witness.coordinates()?;
        if path != record.stage
            || device != record.stage_device
            || inode != record.stage_inode
            || !self.witness.matches_auth_file(path)
        {
            return Err("native publication stage changed".into());
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(super) fn with_original_stage<R>(
        &self,
        use_stage: impl FnOnce(
            &[u8],
            &codex_app_login::StagedWitness,
            &codex_app_login::AuthLock,
            Option<Option<&str>>,
        ) -> Result<R, String>,
    ) -> Result<R, String> {
        self.current()?;
        let predecessor = self
            .issuer
            .record
            .response
            .as_ref()
            .and_then(|response| response.predecessor.as_ref())
            .map(StagePredecessor::digest);
        let result = use_stage(
            self.document.as_slice(),
            &self.witness,
            &self.auth,
            predecessor,
        )?;
        self.current()?;
        Ok(result)
    }

    pub(super) fn initialize_revision(
        &self,
        env: &Env,
        permit: &super::accounts::ActualPublicationPermit<'_>,
        fresh: &RetainedGrant,
    ) -> Result<OwnerWitnessTransition, String> {
        self.current()?;
        if fresh.reference() != self.issuer.reference {
            return Err("native publication issuer changed".into());
        }
        permit.require_staging_store(self.issuer.staging())?;
        if !codex_app_login::retained_document_current(
            &self.witness,
            &self.auth,
            self.document.as_slice(),
        ) {
            return Err("native publication document differs from issuer".into());
        }
        let binding = permit.binding(&self.issuer.record.request, fresh)?;
        let transition =
            initialize_fresh_revision(env, &binding, &self.identity, self.document.as_slice())?;
        catalogue::mark_owner(
            env,
            &binding,
            &self.witness,
            &self.auth,
            self.document.as_slice(),
        )?;
        self.current()?;
        if !codex_app_login::retained_document_current(
            &self.witness,
            &self.auth,
            self.document.as_slice(),
        ) {
            return Err("native publication document changed".into());
        }
        Ok(transition)
    }
}

impl RowPublicationIssuer {
    pub(super) fn publication_response(&self) -> Result<PublicationResponse<'_>, String> {
        self.owner.current()?;
        let witness = codex_app_login::StagedWitness::open(&self.record.stage)?;
        let (path, device, inode) = witness.coordinates()?;
        if path != self.record.stage
            || device != self.record.stage_device
            || inode != self.record.stage_inode
        {
            return Err("native publication stage parent changed".into());
        }
        let auth = codex_app_login::AuthLock::try_take(path)
            .map_err(|_| "native publication stage custody unavailable")?
            .ok_or("native publication stage custody busy")?;
        let response = self
            .record
            .response
            .as_ref()
            .ok_or("native response absent")?;
        let held = PublicationResponse {
            issuer: self,
            identity: response.identity.clone().ok_or("native identity absent")?,
            document: decoded_document(response, MAX_DOCUMENT)?,
            witness,
            auth,
        };
        held.current()?;
        Ok(held)
    }

    pub(super) fn reference(&self) -> GrantReference {
        self.reference.clone()
    }

    pub(super) fn staging(&self) -> &codex_enrollment::Staging {
        self.record
            .response
            .as_ref()
            .and_then(|response| response.native_row.as_ref())
            .expect("issuer constructor requires native staging")
    }
    pub(super) fn sign_publication(
        &self,
        permit: &super::accounts::ActualPublicationPermit<'_>,
        fresh: &RetainedGrant,
    ) -> Result<RowPublicationSignature, String> {
        permit.require_staging_store(self.staging())?;
        let binding = permit.binding(&self.record.request, fresh)?;
        let fingerprint = permit.after_binding_digest(&self.record.request, fresh)?;
        let staging = self.staging().clone();
        let mac = self.authenticate_receipt(&binding, &staging, &fingerprint)?;
        Ok(RowPublicationSignature {
            stamp: permit.stamp(),
            binding,
            staging,
            fingerprint,
            mac,
        })
    }

    pub(super) fn verify_publication(
        &self,
        data: &super::codex_native_row::PublicationReceiptData,
    ) -> Result<(), String> {
        if !data.valid() {
            return Err("native publication receipt is invalid".into());
        }
        let expected =
            self.authenticate_receipt(data.binding(), data.staging(), data.binding_fingerprint())?;
        if !aterm_digest::ct_eq(expected.as_bytes(), data.seal().as_bytes()) {
            return Err("native publication receipt seal differs".into());
        }
        Ok(())
    }

    fn authenticate_receipt(
        &self,
        binding: &GrantBinding,
        staging: &codex_enrollment::Staging,
        fingerprint: &str,
    ) -> Result<String, String> {
        self.owner.current()?;
        if binding.request != self.record.request
            || binding.reference != self.reference
            || binding.generation == 0
            || staging != self.staging()
        {
            return Err("native row issuer does not match publication".into());
        }
        let body = aterm_json::to_vec(&(
            "aterm/native-row-publication/v1",
            binding,
            staging,
            fingerprint,
        ))
        .map_err(|_| "native publication receipt cannot be encoded")?;
        machine_mac(
            &self.owner.identity,
            b"aterm/native-row-publication/key/v1\0",
            &body,
        )
    }
}

/// A single actual permit's signed occurrence. No serialized-data constructor,
/// cloning or replacement stamp; consuming it retains the original channel.
pub(super) struct RowPublicationSignature {
    stamp: super::accounts::ActualPublicationStamp,
    binding: GrantBinding,
    staging: codex_enrollment::Staging,
    fingerprint: String,
    mac: String,
}
impl RowPublicationSignature {
    pub(super) fn into_parts(
        self,
    ) -> (
        super::accounts::ActualPublicationStamp,
        GrantBinding,
        codex_enrollment::Staging,
        String,
        String,
    ) {
        (
            self.stamp,
            self.binding,
            self.staging,
            self.fingerprint,
            self.mac,
        )
    }
}

pub(super) fn admit_native_recovery_publication(
    env: &Env,
    store: &super::accounts::Store,
    candidate: super::codex_native_row::NativeRecoveryCandidate,
    fresh: &RetainedGrant,
    scope: &super::account_work::Scope,
    deadline: super::wake_deadline::WakeDeadline,
) -> Result<super::native_enrollment_admission::NativeRecoveryResources, String> {
    if !fresh.matches_request(candidate.request())
        || fresh.reference() != *candidate.reference()
        || std::fs::canonicalize(
            env.home
                .as_deref()
                .ok_or("native recovery home unavailable")?,
        )
        .ok()
        .as_deref()
            != Some(candidate.source_home())
    {
        return Err("native recovery response or namespace changed".into());
    }
    // This bounded observation releases its actual owner/Auth claims before
    // acquiring the higher-ranked Source/E/principal resources.
    let identity = publication_identity(env, candidate.request(), fresh)?;
    let turn = super::native_enrollment_admission::NativeRecoveryResources::acquire(
        store, candidate, &identity, scope, deadline,
    )?;
    let result = publication_identity(env, turn.candidate().request(), fresh).and_then(|current| {
        if current != identity {
            return Err("native recovery response changed".into());
        }
        turn.check_store(store)
    });
    result?;
    Ok(turn)
}

pub(super) fn admit_native_retirement(
    env: &Env,
    store: &super::accounts::Store,
    candidate: super::codex_native_retirement::NativeRetirementCandidate,
    fresh: &RetainedGrant,
    scope: &super::account_work::Scope,
    release: super::account_work::ReleaseTurn,
    deadline: super::wake_deadline::WakeDeadline,
) -> Result<super::native_enrollment_admission::NativeRetirementResources, String> {
    if !fresh.matches_request(candidate.request())
        || fresh.reference() != *candidate.reference()
        || std::fs::canonicalize(
            env.home
                .as_deref()
                .ok_or("native retirement home unavailable")?,
        )
        .ok()
        .as_deref()
            != Some(candidate.source_home())
    {
        return Err("native retirement response or namespace changed".into());
    }
    // Historical issuer custody releases before kernel waits. No stage or
    // AuthLock is opened: this role can only retire canonical metadata.
    let identity = retirement_identity(env, &candidate, fresh)?;
    let resources = super::native_enrollment_admission::NativeRetirementResources::acquire(
        store, candidate, &identity, scope, release, deadline,
    )?;
    if retirement_identity(env, resources.candidate(), fresh)? != identity {
        return Err("native retirement response changed".into());
    }
    resources.check_store(store)?;
    Ok(resources)
}

pub(super) fn admit_native_retirement_released(
    env: &Env,
    store: &super::accounts::Store,
    candidate: super::codex_native_retirement::NativeRetirementCandidate,
    fresh: &RetainedGrant,
    operation: &super::account_work::ReleaseOperation,
    deadline: super::wake_deadline::WakeDeadline,
) -> Result<super::native_enrollment_admission::NativeRetirementResources, String> {
    if !fresh.matches_request(candidate.request())
        || fresh.reference() != *candidate.reference()
        || std::fs::canonicalize(
            env.home
                .as_deref()
                .ok_or("native retirement home unavailable")?,
        )
        .ok()
        .as_deref()
            != Some(candidate.source_home())
    {
        return Err("native retirement response or namespace changed".into());
    }
    // Historical issuer custody releases before kernel waits. No stage or
    // AuthLock is opened: this role can only retire canonical metadata.
    let identity = retirement_identity(env, &candidate, fresh)?;
    let resources =
        super::native_enrollment_admission::NativeRetirementResources::acquire_released(
            store, candidate, &identity, operation, deadline,
        )?;
    if retirement_identity(env, resources.candidate(), fresh)? != identity {
        return Err("native retirement response changed".into());
    }
    resources.check_store(store)?;
    Ok(resources)
}

pub(super) fn row_publication_issuer(
    env: &Env,
    request: &codex_enrollment::Request,
    fresh: &RetainedGrant,
) -> Result<RowPublicationIssuer, String> {
    if !fresh.matches_request(request) {
        return Err("native publication request differs".into());
    }
    let (owner, record) = open_authenticated(env, request, &fresh.reference)?;
    if !matches!(record.schema, 6 | 7)
        || record
            .response
            .as_ref()
            .and_then(|r| r.native_row.as_ref())
            .is_none()
    {
        return Err("native publication has no row issuer".into());
    }
    Ok(RowPublicationIssuer {
        owner,
        record,
        reference: fresh.reference(),
    })
}

/// Observe the exact authenticated retained response while its original
/// opened stage parent and AuthLock remain held. This does not adopt or rewrite
/// any existing stage document; a caller must prove its own write transition.
/// The last callback argument is unknown for legacy records, or an authenticated
/// observation of an absent predecessor / an exact pre-POST digest.
pub(super) fn with_retained_response<R>(
    env: &Env,
    request: &codex_enrollment::Request,
    grant: &RetainedGrant,
    use_response: impl FnOnce(
        &Identity,
        &[u8],
        &codex_app_login::StagedWitness,
        &codex_app_login::AuthLock,
        Option<Option<&str>>,
    ) -> Result<R, String>,
) -> Result<R, String> {
    if !grant.matches_request(request) {
        return Err("retained grant request differs".into());
    }
    let (owner, record) = open_authenticated(env, request, &grant.reference)?;
    let witness = codex_app_login::StagedWitness::open(&record.stage)?;
    let (path, device, inode) = witness.coordinates()?;
    if path != record.stage || device != record.stage_device || inode != record.stage_inode {
        return Err("retained stage parent changed".into());
    }
    let auth = codex_app_login::AuthLock::try_take(path)
        .map_err(|_| "retained stage custody unavailable")?
        .ok_or("retained stage custody busy")?;
    let response = record.response.as_ref().ok_or("retained response absent")?;
    let identity = response
        .identity
        .as_ref()
        .ok_or("retained identity absent")?;
    let document = decoded_document(response, MAX_DOCUMENT)?;
    owner.current()?;
    let result = use_response(
        identity,
        document.as_slice(),
        &witness,
        &auth,
        response.predecessor.as_ref().map(StagePredecessor::digest),
    )?;
    owner.current()?;
    if !witness.matches_auth_file(path) {
        return Err("retained stage parent changed".into());
    }
    Ok(result)
}

/// Publication-only initialization while the original retained response and
/// stage AuthLock remain held by `with_retained_response`.
pub(super) struct FreshPublicationResponse<'a> {
    env: &'a Env,
    request: &'a codex_enrollment::Request,
    grant: &'a RetainedGrant,
    identity: &'a Identity,
    document: &'a [u8],
    witness: &'a codex_app_login::StagedWitness,
    auth: &'a codex_app_login::AuthLock,
}
impl FreshPublicationResponse<'_> {
    pub(super) fn initialize(
        &self,
        permit: &super::accounts::ActualPublicationPermit<'_>,
    ) -> Result<OwnerWitnessTransition, String> {
        if !codex_app_login::retained_document_current(self.witness, self.auth, self.document) {
            return Err("native publication document differs from issuer".into());
        }
        let binding = permit.binding(self.request, self.grant)?;
        let transition =
            initialize_fresh_revision(self.env, &binding, self.identity, self.document)?;
        catalogue::mark_owner(self.env, &binding, self.witness, self.auth, self.document)?;
        if !codex_app_login::retained_document_current(self.witness, self.auth, self.document) {
            return Err("native publication document changed".into());
        }
        Ok(transition)
    }
}

pub(super) fn with_fresh_publication_response<R>(
    env: &Env,
    request: &codex_enrollment::Request,
    grant: &RetainedGrant,
    publish: impl FnOnce(FreshPublicationResponse<'_>) -> Result<R, String>,
) -> Result<R, String> {
    with_retained_response(
        env,
        request,
        grant,
        |identity, document, witness, auth, _| {
            publish(FreshPublicationResponse {
                env,
                request,
                grant,
                identity,
                document,
                witness,
                auth,
            })
        },
    )
}

/// Revalidate the actual canonical publication, current source policy and
/// consumer custody before exposing native bytes to a session materializer.
/// Destination quota readiness is a separate natural-use assignment admission;
/// an expired or limited source is not denied permission to leave its account.
/// An immutable native launch choice, not materialization authority. The
/// actual consumer and authenticated owner are joined again before delivery.
pub(super) struct CanonicalLaunchChoice {
    binding: GrantBinding,
    auth_file: PathBuf,
}
impl CanonicalLaunchChoice {
    pub(super) fn binding(&self) -> &GrantBinding {
        &self.binding
    }
    pub(super) fn auth_file(&self) -> &Path {
        &self.auth_file
    }
}

pub(super) fn capture_canonical_launch_choice(
    env: &Env,
    auth_file: &Path,
    source_work: &super::codex_source_gate::SourceWorkLease,
) -> Result<Option<CanonicalLaunchChoice>, String> {
    use super::accounts;
    source_work.current()?;
    if !auth_file.is_absolute() {
        return Err("native launch source is relative".into());
    }
    let Some(roster) = env.accounts_roster_for(accounts::Cli::Codex) else {
        return if codex_app_login::legacy_file_reader_allowed(auth_file) {
            Ok(None)
        } else {
            Err("native-owned launch source has no active configuration".into())
        };
    };
    let store = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("native account store unavailable")?;
    let _writer = store
        .acquire_bounded()
        .map_err(|_| "native account writer unavailable")?
        .ok_or("native account writer busy")?;
    let records = store
        .peek_account_records(&roster, env.now)
        .map_err(|_| "native account publication unavailable")?;
    let home = env.home.as_deref().ok_or("native profile unavailable")?;
    let mut selected = None;
    let mut matched = false;
    for row in &records.roster.rows {
        let candidate = row
            .dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"))
            .join("auth.json");
        if candidate != auth_file {
            continue;
        }
        if matched {
            return Err("native launch source is ambiguous".into());
        }
        matched = true;
        let Some(bound) = records.logins.get(&row.label) else {
            continue;
        };
        let Some(binding) = records
            .logins
            .enrollment
            .published_grant(&row.label, bound.last_success_generation)
        else {
            continue;
        };
        if !records
            .logins
            .enrollment
            .authenticates_published_grant(&binding)
        {
            return Err("native launch grant publication changed".into());
        }
        selected = Some(CanonicalLaunchChoice {
            binding,
            auth_file: candidate,
        });
    }
    source_work.current()?;
    Ok(selected)
}

pub(super) fn with_current_published_grant<R>(
    env: &Env,
    binding: &GrantBinding,
    caller: &super::codex_session_slot::Caller,
    use_grant: impl FnOnce(AdmittedGrant<'_>) -> Result<R, String>,
) -> Result<R, String> {
    use super::native_act::Process;
    let process = Process::observe(caller.pid).ok_or("credential consumer unavailable")?;
    if process.birth != caller.birth || !consumer_capable(&process, false)? {
        return Err("credential consumer unsupported".into());
    }
    with_canonical_owner(
        env,
        binding,
        |grant| {
            catalogue::require_participant(env, grant.binding, grant.identity)?;
            lineage_consumers_capable(env, grant.canonical_auth, grant.identity)?;
            if Process::observe(caller.pid).as_ref() != Some(&process) {
                return Err("credential consumer changed".into());
            }
            Ok(())
        },
        |grant| {
            let result = use_grant(AdmittedGrant {
                binding: grant.binding,
                identity: grant.identity,
                revision: grant.revision,
                document: grant.document,
                canonical_auth: grant.canonical_auth,
                anchor: grant.anchor,
            })?;
            if Process::observe(caller.pid).as_ref() != Some(&process) {
                return Err("credential consumer changed".into());
            }
            Ok(result)
        },
    )
}

/// Resolve one target from the same canonical publication used by admission.
/// No global pool pin, sign-in, provider request or assignment is changed.
pub(super) fn with_selected_target<R>(
    env: &Env,
    caller: &super::codex_session_slot::Caller,
    pool_revision: &str,
    selection: &super::codex_assignment::protocol::Selection,
    source: &GrantBinding,
    retained: Option<&GrantBinding>,
    use_grant: impl FnOnce(AdmittedGrant<'_>) -> Result<R, String>,
) -> Result<R, String> {
    use super::{accounts, codex_assignment::protocol::Selection, native_act::Process};
    let process = Process::observe(caller.pid).ok_or("credential consumer unavailable")?;
    if process.birth != caller.birth || !consumer_capable(&process, false)? {
        return Err("credential consumer unsupported".into());
    }
    let store = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("account store unavailable")?;
    let writer = store
        .acquire_bounded()
        .map_err(|_| "account writer unavailable")?
        .ok_or("account writer busy")?;
    let roster_path = env
        .accounts_roster_for(accounts::Cli::Codex)
        .ok_or("account roster unavailable")?;
    let records = store
        .peek_account_records(&roster_path, env.now)
        .map_err(|_| "account publication unavailable")?;
    if accounts::pool_authority_revision(&records.raw_roster, &records.logins)? != pool_revision {
        return Err("assignment pool revision changed".into());
    }
    if !records
        .logins
        .enrollment
        .authenticates_published_grant(source)
    {
        return Err("assignment source publication changed".into());
    }
    let source_scope = catalogue::existing_scope(env, &source.request.id)?
        .ok_or("historical assignment source requires authenticated migration")?;
    if source_scope.request() != &source.request {
        return Err("assignment source catalogue scope changed".into());
    }
    let source_row = records
        .roster
        .row(&source.request.label)
        .ok_or("assignment source unavailable")?;
    let source_policy = super::accounts_sync::codex_completion_source(env, source_row)
        .map_err(|_| "assignment source policy unavailable")?;
    if source_row.held_by.is_some()
        || source_policy.held_by.is_some()
        || source_policy.fleet.as_ref().is_some_and(|fleet| {
            fleet.roster.accounts.iter().any(|account| {
                account.label == source_row.label
                    && (account.vendor != source_row.vendor
                        || !source_row
                            .expected_identity_email()
                            .is_some_and(|email| accounts::email_matches(email, &account.email)))
            })
        })
    {
        return Err("assignment source policy changed".into());
    }
    let enrolled = store.enrolled_here(&records.roster);
    let candidates = accounts::candidates(
        &records.roster,
        &records.state,
        &enrolled,
        None,
        &accounts::Point::TurnEnd,
        env.now,
    );
    let mut selected = None;
    for (label, eligible) in candidates {
        if eligible.is_err() {
            continue;
        }
        let matches = match selection {
            Selection::Auto => label != source.request.label,
            Selection::Account(id) => super::codex_enrollment::account_id(&label) == *id,
        };
        if !matches {
            continue;
        }
        let Some(bound) = records.logins.get(&label) else {
            continue;
        };
        let Some(binding) = records
            .logins
            .enrollment
            .published_grant(&label, bound.last_success_generation)
        else {
            continue;
        };
        if retained.is_some_and(|expected| expected != &binding) {
            continue;
        }
        if matches!(selection, Selection::Account(_)) && selected.is_some() {
            return Err("assignment target is ambiguous".into());
        }
        selected = Some(binding);
        if matches!(selection, Selection::Auto) {
            break;
        }
    }
    let binding = selected.ok_or("assignment target unavailable")?;
    let result = with_canonical_owner_held(
        env,
        &binding,
        CanonicalContext {
            store: &store,
            writer: &writer,
            roster_path: &roster_path,
            records: &records,
        },
        |grant| {
            catalogue::require_participant(env, grant.binding, grant.identity)?;
            lineage_consumers_capable(env, grant.canonical_auth, grant.identity)?;
            if Process::observe(caller.pid).as_ref() != Some(&process) {
                return Err("credential consumer changed".into());
            }
            Ok(())
        },
        |grant| {
            let result = use_grant(AdmittedGrant {
                binding: grant.binding,
                identity: grant.identity,
                revision: grant.revision,
                document: grant.document,
                canonical_auth: grant.canonical_auth,
                anchor: grant.anchor,
            })?;
            if Process::observe(caller.pid).as_ref() != Some(&process) {
                return Err("credential consumer changed".into());
            }
            Ok(result)
        },
    )?;
    if super::accounts_sync::codex_completion_source(env, source_row)
        .map_err(|_| "assignment source policy unavailable")?
        != source_policy
    {
        return Err("assignment source policy changed".into());
    }
    Ok(result)
}

fn with_canonical_owner<R>(
    env: &Env,
    binding: &GrantBinding,
    before_use: impl FnOnce(&CanonicalGrant<'_>) -> Result<(), String>,
    use_owner: impl FnOnce(CanonicalGrant<'_>) -> Result<R, String>,
) -> Result<R, String> {
    use super::accounts;
    if binding.generation == 0 || !binding.request.valid() || !binding.reference.valid() {
        return Err("invalid published grant binding".into());
    }
    let store = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("account store unavailable")?;
    let _writer = store
        .acquire_bounded()
        .map_err(|_| "account writer unavailable")?
        .ok_or("account writer busy")?;
    let roster_path = env
        .accounts_roster_for(accounts::Cli::Codex)
        .ok_or("account roster unavailable")?;
    let records = store
        .peek_account_records(&roster_path, env.now)
        .map_err(|_| "account publication unavailable")?;
    with_canonical_owner_held(
        env,
        binding,
        CanonicalContext {
            store: &store,
            writer: &_writer,
            roster_path: &roster_path,
            records: &records,
        },
        before_use,
        use_owner,
    )
}

struct CanonicalContext<'a> {
    store: &'a super::accounts::Store,
    writer: &'a super::accounts::Lock,
    roster_path: &'a Path,
    records: &'a super::accounts::AccountRecords,
}

fn with_canonical_owner_held<R>(
    env: &Env,
    binding: &GrantBinding,
    context: CanonicalContext<'_>,
    before_use: impl FnOnce(&CanonicalGrant<'_>) -> Result<(), String>,
    use_owner: impl FnOnce(CanonicalGrant<'_>) -> Result<R, String>,
) -> Result<R, String> {
    use super::{accounts, accounts_sync};
    let CanonicalContext {
        store,
        writer,
        roster_path,
        records,
    } = context;
    if !records
        .logins
        .enrollment
        .authenticates_published_grant(binding)
    {
        return Err("grant is not canonically published".into());
    }
    let row = records
        .roster
        .row(&binding.request.label)
        .ok_or("published row unavailable")?;
    let bound = records
        .logins
        .get(&row.label)
        .ok_or("published binding unavailable")?;
    if bound.last_success_generation != binding.generation || row.held_by.is_some() {
        return Err("published binding changed".into());
    }
    let source = accounts_sync::codex_completion_source(env, row)
        .map_err(|_| "account source unavailable")?;
    if source.held_by.is_some()
        || source.fleet.as_ref().is_some_and(|fleet| {
            fleet.roster.accounts.iter().any(|account| {
                account.label == row.label
                    && (account.vendor != row.vendor
                        || !row
                            .expected_identity_email()
                            .is_some_and(|email| accounts::email_matches(email, &account.email)))
            })
        })
    {
        return Err("account source changed".into());
    }
    let home = env.home.as_deref().ok_or("account home unavailable")?;
    let canonical_auth = row
        .dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
        .join("auth.json");
    let publication = published_source::PublishedSource::authenticate(
        env,
        binding,
        &records.logins,
        &canonical_auth,
    )?;
    let (owner, issuance) = open_authenticated(env, &binding.request, &binding.reference)?;
    if issuance.catalogue_scope.is_some() && publication.is_none() {
        return Err("global owner has no authenticated canonical publication".into());
    }
    let anchor = codex_enrollment::OwnerAnchorWriter::new(store, writer, binding, env.now);
    let witness = anchor
        .read()?
        .ok_or("published owner has no canonical activation witness")?;
    let current = refresh::current_revision(env, binding, &issuance, &witness)?;
    let expected_identity = issuance
        .response
        .as_ref()
        .and_then(|response| response.identity.as_ref())
        .ok_or("grant identity unavailable")?;
    if current.identity != *expected_identity
        || current.revision.generation == 0
        || !valid_hex(&current.revision.digest, 64)
        || current.document.len() > MAX_DOCUMENT
        || aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
            current.document.as_slice(),
        )) != current.revision.digest
    {
        return Err("owned grant revision changed".into());
    }
    let _auth = codex_app_login::AuthLock::try_take(&canonical_auth)
        .map_err(|_| "native auth custody unavailable")?
        .ok_or("native auth custody busy")?;
    let parent = canonical_auth
        .parent()
        .ok_or("native auth location invalid")?;
    let bytes = platform::read_single_link_private(parent, "auth.json", MAX_DOCUMENT)
        .map_err(|_| "native auth document unavailable")?;
    if !current.canonical_matches(bytes.as_slice())
        || codex_app_login::renewal_identity(parent).as_ref() != Some(&current.identity)
    {
        return Err("native auth document differs from owned revision".into());
    }
    let grant = CanonicalGrant {
        binding,
        identity: &current.identity,
        revision: &current.revision,
        document: current.document.as_slice(),
        canonical_auth: &canonical_auth,
        anchor,
    };
    before_use(&grant)?;
    if accounts_sync::codex_completion_source(env, row).map_err(|_| "account source unavailable")?
        != source
    {
        return Err("grant admission changed".into());
    }
    owner.current()?;
    let result = use_owner(grant)?;
    owner.current()?;
    let after = store
        .peek_account_records(roster_path, env.now)
        .map_err(|_| "account publication unavailable")?;
    if let Some(publication) = &publication {
        publication.at(&after.logins, &canonical_auth)?;
    }
    if after.raw_roster != records.raw_roster
        || !after
            .logins
            .enrollment
            .authenticates_published_grant(binding)
        || accounts_sync::codex_completion_source(env, row)
            .map_err(|_| "account source unavailable")?
            != source
    {
        return Err("canonical owner source changed".into());
    }
    Ok(result)
}

/// Capability is checked against the actual mapped file, with process identity
/// bracketed around the read. A path or a copied capability string is no grant.
pub(super) fn consumer_capable(
    process: &super::native_act::Process,
    source_domain: bool,
) -> Result<bool, String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::MetadataExt as _;
        let path = Path::new(std::ffi::OsStr::from_bytes(&process.image.path));
        let metadata = std::fs::metadata(path).map_err(|_| "consumer image unavailable")?;
        if metadata.dev() != process.image.device
            || metadata.ino() != process.image.inode
            || !super::codex_alab::alab_bytes(path)
        {
            return Ok(false);
        }
        let supported = super::codex_alab::managed_credential_capable(path, source_domain)
            .map_err(|_| "consumer capability unavailable")?;
        Ok(supported && super::native_act::Process::observe(process.pid).as_ref() == Some(process))
    }
    #[cfg(not(unix))]
    {
        let _ = (process, source_domain);
        Err("native credential consumer unsupported".into())
    }
}

/// Known native consumers are classified from kernel-owned exec environments;
/// an unread live Codex consumer keeps custody unresolved instead of being
/// treated as absent. No provider contact or credential publication occurs here.
fn lineage_consumers_capable(
    env: &Env,
    canonical_auth: &Path,
    identity: &Identity,
) -> Result<(), String> {
    let records = aterm_sysprobe::kproc::records().ok_or("consumer census unavailable")?;
    if records.len() >= aterm_sysprobe::procs::MAX_PROCESSES {
        return Err("consumer census is incomplete".into());
    }
    #[cfg(unix)]
    let uid = unsafe { libc::geteuid() }; // SAFETY: no arguments, no failure mode.
    #[cfg(not(unix))]
    let uid = 0;
    for record in records.iter().filter(|record| {
        record.uid == uid && !record.zombie && record.comm.to_ascii_lowercase().contains("codex")
    }) {
        let before =
            super::native_act::Process::observe(record.pid).ok_or("live consumer unreadable")?;
        let args = atpkg::caller_shell::process_args(record.pid)
            .ok_or("live consumer environment unreadable")?;
        let home = args
            .env_var(super::codex_alab::HOME_ENV)
            .map(PathBuf::from)
            .or_else(|| env.home.as_ref().map(|home| home.join(".codex")))
            .ok_or("live consumer home unknown")?;
        let auth = args
            .env_var(super::codex_alab::AUTH_PATH_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("auth.json"));
        if !auth.is_absolute() {
            return Err("live consumer source unknown".into());
        }
        let same_source = auth == canonical_auth
            || auth
                .parent()
                .and_then(codex_app_login::renewal_identity)
                .is_some_and(|other| other.user == identity.user);
        if same_source && !consumer_capable(&before, false)? {
            return Err("an older live consumer retains this grant lineage".into());
        }
        if super::native_act::Process::observe(record.pid).as_ref() != Some(&before) {
            return Err("consumer census changed".into());
        }
    }
    Ok(())
}

// The only fixture constructor owns its fresh private world; no caller path,
// response document, grant, or existing account authority can be supplied.
#[cfg(all(unix, any(test, debug_assertions)))]
#[path = "codex_grant_owner_row_fixture.rs"]
mod row_fixture;
#[cfg(all(unix, any(test, debug_assertions)))]
pub(super) use row_fixture::create as native_recovery_row_fixture;
#[cfg(all(test, unix))]
pub(super) use row_fixture::create_five as native_recovery_five_fixture;
#[cfg(all(unix, any(test, debug_assertions)))]
pub(super) use row_fixture::create_source_host as native_source_host_fixture;

#[cfg(all(test, unix))]
#[path = "codex_grant_owner_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "codex_grant_maintenance_tests.rs"]
mod maintenance_tests;

#[cfg(all(test, unix))]
pub(in crate::harness) fn with_global_code_fixture(
    test: impl FnOnce(
        &Env,
        &codex_enrollment::Request,
        &Path,
        std::sync::Arc<super::native_enrollment_admission::NativeBrowserTurn>,
        PreparedGrant,
        codex_enrollment::Artifact,
    ),
) {
    row_fixture::with_global_code_fixture(test);
}

/// Exact authenticated fixture occurrence locator; never owner adoption from a path.
#[cfg(all(test, unix))]
pub(in crate::harness) fn fixture_owner_cipher(
    env: &Env,
    request: &codex_enrollment::Request,
    grant: &RetainedGrant,
) -> Result<PathBuf, String> {
    let (owner, _) = open_authenticated(env, request, &grant.reference())?;
    owner.current()?;
    Ok(owner.directory.join("candidate.age"))
}

#[cfg(all(test, unix))]
pub(in crate::harness) fn with_code_rejection_fixture(
    test: impl FnOnce(&Env, &Path, &codex_enrollment::Request),
) {
    let (_world, mut env, stage, request) = tests::fixture(true);
    let config = env.home.as_ref().unwrap().join("config");
    platform::ensure_private_dir(&config).unwrap();
    let path = config.join("aterm.toml");
    std::fs::write(&path, "").unwrap();
    env.config = Some(path);
    std::fs::write(env.accounts_roster_for(super::accounts::Cli::Codex).unwrap(),
        format!("[[account]]\nlabel='fixture'\nemail='fixture@example.test'\nclass='dedicated'\ndir='{}'\n", stage.parent().unwrap().display())).unwrap();
    test(&env, &stage, &request);
}

#[cfg(all(test, unix))]
#[path = "codex_initial_source_tests.rs"]
mod initial_source_tests;

#[cfg(all(test, unix))]
pub(in crate::harness) type NativeRowFixture = (
    codex_enrollment::Request,
    codex_enrollment::Staging,
    RetainedGrant,
    Option<codex_enrollment::Artifact>,
);

/// Synthetic encrypted issuer inputs for actual writer/adapter tests. This
/// supplies no provider, independent-lineage or consumer-readiness proof.
#[cfg(all(test, unix))]
pub(in crate::harness) fn with_two_native_rows_fixture(
    test: impl FnOnce(&Env, &super::accounts::Store, &Path, [NativeRowFixture; 2]),
) {
    tests::with_two_native_rows_fixture(test)
}

/// Exact synthetic response retention cut before canonical attachment.
#[cfg(all(test, unix))]
pub(in crate::harness) fn with_unattached_native_rows_fixture(
    test: impl FnOnce(&Env, &super::accounts::Store, &Path, [NativeRowFixture; 2]),
) {
    tests::with_native_rows_at_attachment_cut(false, test)
}

#[cfg(all(test, unix))]
pub(in crate::harness) use refresh::fixture_promote_received_response;

#[cfg(test)]
mod metadata_tests {
    use super::*;

    #[test]
    fn grant_reference_codec_is_portable_strict_and_redacted() {
        let value = aterm_json::json!({
            "operation": "a".repeat(32), "capability": "b".repeat(64),
            "issuance_digest": "c".repeat(64), "producer_mac": "d".repeat(64)
        });
        let reference: GrantReference = aterm_json::from_value(value.clone()).unwrap();
        assert!(reference.valid());
        assert_eq!(aterm_json::to_value(&reference).unwrap(), value);
        assert!(!format!("{reference:?}").contains(&"b".repeat(64)));
        let mut unknown = value;
        unknown
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), true.into());
        assert!(aterm_json::from_value::<GrantReference>(unknown).is_err());
    }
}
