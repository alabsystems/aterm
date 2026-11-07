// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Encrypted custody of an enrollment's irreversible OAuth response. A retained
//! response is not a published account, independent grant, or session assignment.
//! Only the successful exchange producer can supply verified response evidence.

use super::{cli::Env, codex_app_login, codex_enrollment, codex_renewal::Identity};
use aterm_secrets::{SecretBytes, age, client};
use aterm_uds::private_file::{self, PrivateLock};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroize::Zeroize;

const MAX_DOCUMENT: usize = 16 * 1024;
// A completed unsupported response must still survive. This is the native
// reader's existing document bound, not permission to materialize a large slot.
const MAX_RETAINED_DOCUMENT: usize = 2 * 1024 * 1024;
const MAX_ENVELOPE: usize = 4 * 1024 * 1024;
const MAX_PENDING: usize = 256;

#[path = "codex_grant_refresh.rs"]
mod refresh;
#[cfg(test)]
pub(super) use refresh::initialize_revision;
pub(super) use refresh::{
    OwnedRefreshRetention, OwnerRefreshContinuation, OwnerRefreshError, OwnerWitnessTransition,
    with_refreshed_owner,
};

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
        ((self.schema == 1
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
    private_file::check_private_dir(parent)
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
    journal: super::pending_native_store::Store,
    directory: PathBuf,
    home: PathBuf,
    identity: age::Identity,
}

impl Store {
    fn location(env: &Env, id: &str) -> Result<(PathBuf, PathBuf), String> {
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

    fn open(env: &Env, id: &str, prepare: bool) -> Result<Option<(Self, Option<Record>)>, String> {
        let (directory, home) = Self::location(env, id)?;
        let identity = client::read_machine_identity(&client::Paths::locate_for_home(&home))
            .map_err(|_| "grant owner identity unavailable")?
            .ok_or("grant owner identity unavailable")?;
        let opened = if prepare {
            Some(super::pending_native_store::Store::open_owned(
                &directory,
                &home,
                MAX_ENVELOPE,
            )?)
        } else {
            super::pending_native_store::Store::open_owned_existing(
                &directory,
                &home,
                MAX_ENVELOPE,
            )?
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
                if !record.valid() || record.request.id != id {
                    return Err("grant custody record is invalid");
                }
                Ok(record)
            })
            .transpose()?;
        Ok(Some((
            Self {
                journal,
                directory,
                home,
                identity,
            },
            record,
        )))
    }

    fn current(&self) -> Result<(), String> {
        let identity = client::read_machine_identity(&client::Paths::locate_for_home(&self.home))
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
            let identity =
                client::read_machine_identity(&client::Paths::locate_for_home(&self.home))
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
        if let Some(predecessor) = &response.predecessor {
            let body = SecretBytes::new(
                aterm_json::to_vec(&("aterm/codex-code-response-origin/v2", legacy, predecessor))
                    .map_err(|_| "grant origin cannot be encoded")?,
            );
            machine_mac(
                &self.identity,
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
                &self.identity,
                b"aterm/codex-code-response-origin/key/v1\0",
                body.as_slice(),
            )
        }
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

/// One staged exchange's durable custody, prepared before the authorization code
/// can be spent. Different enrollments hold different locks.
pub(super) struct PreparedGrant {
    store: Store,
    record: Record,
    stage_witness: codex_app_login::StagedWitness,
    fresh_origin: bool,
    no_dispatch_pending: bool,
    rejection_origin: bool,
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
    if !request.valid() || !valid_hex(source_fence, 64) {
        return Err("invalid grant preparation".into());
    }
    let stage = stage_path(stage)?;
    let stage_witness = codex_app_login::StagedWitness::open(&stage)?;
    let (_, stage_device, stage_inode) = stage_witness.coordinates()?;
    let (directory, _) = Store::location(env, &request.id)?;
    let root = directory
        .parent()
        .ok_or("grant custody directory unavailable")?;
    aterm_secrets::fsx::ensure_private_dir(root)
        .map_err(|_| "grant custody directory unavailable")?;
    let _registry = PrivateLock::take(root, "registry.lock", Duration::ZERO, "grant registry busy")
        .map_err(|_| "grant registry is busy")?;
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
        store,
        record,
        stage_witness,
        fresh_origin: false,
        no_dispatch_pending: false,
        rejection_origin: false,
    })
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
    #[cfg(test)]
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

impl PreparedGrant {
    fn settle_not_dispatched(mut self) -> CaptureOutcome {
        self.no_dispatch_pending = true;
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
        CaptureOutcome::NotDispatched(NotDispatched {
            request: self.record.request.clone(),
        })
    }

    pub(super) fn begin_exchange(&mut self) -> Result<(), String> {
        if self.record.phase != Phase::Prepared {
            return Err("grant exchange is already attempted".into());
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
                    .is_some_and(|parent| private_file::check_private_dir(parent).is_ok())
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
        });
        // Only the live FreshCodeGrant callback supplies a verified identity
        // in production. Encrypted recovery never constructs this volatile fact.
        self.fresh_origin = self.record.phase == Phase::VerifiedResponse;
    }

    fn settle_response(mut self) -> CaptureOutcome {
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
        if self.record.phase == Phase::RejectedResponse {
            if !self.store.authenticates_rejection(&self.record) {
                return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
            }
            CaptureOutcome::Rejected(RejectedCompletion {
                _request: self.record.request.clone(),
            })
        } else if self.record.phase == Phase::VerifiedResponse {
            match self.store.issuance(&self.record) {
                Ok(reference) => CaptureOutcome::Verified(RetainedGrant {
                    reference,
                    request: self.record.request.clone(),
                }),
                Err(_) => CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self })),
            }
        } else {
            let Some(digest) = self.store.journal.sealed_digest() else {
                return CaptureOutcome::Pending(Box::new(RetentionFailure { pending: self }));
            };
            CaptureOutcome::Unverified(RetainedResponse {
                _operation: self.record.request.id.clone(),
                _issuance_digest: aterm_codec::hex::encode_lower(&digest),
            })
        }
    }
}

impl codex_app_login::CodeGrantCapture for PreparedGrant {
    type Output = CaptureOutcome;

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

#[cfg(test)]
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

fn with_canonical_owner<R>(
    env: &Env,
    binding: &GrantBinding,
    before_use: impl FnOnce(&CanonicalGrant<'_>) -> Result<(), String>,
    use_owner: impl FnOnce(CanonicalGrant<'_>) -> Result<R, String>,
) -> Result<R, String> {
    use super::{accounts, accounts_sync};
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
    let (owner, issuance) = open_authenticated(env, &binding.request, &binding.reference)?;
    let anchor = codex_enrollment::OwnerAnchorWriter::new(&store, &_writer, binding, env.now);
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
    let home = env.home.as_deref().ok_or("account home unavailable")?;
    let canonical_auth = row
        .dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
        .join("auth.json");
    let _auth = codex_app_login::AuthLock::try_take(&canonical_auth)
        .map_err(|_| "native auth custody unavailable")?
        .ok_or("native auth custody busy")?;
    let parent = canonical_auth
        .parent()
        .ok_or("native auth location invalid")?;
    let (bytes, metadata) =
        private_file::read_private_with_metadata(parent, "auth.json", MAX_DOCUMENT)
            .map_err(|_| "native auth document unavailable")?;
    let bytes = SecretBytes::new(bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return Err("native auth document has aliases".into());
        }
    }
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
        .peek_account_records(&roster_path, env.now)
        .map_err(|_| "account publication unavailable")?;
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

#[cfg(test)]
#[path = "codex_grant_owner_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "codex_grant_maintenance_tests.rs"]
mod maintenance_tests;

#[cfg(test)]
pub(in crate::harness) fn with_code_rejection_fixture(
    test: impl FnOnce(&Env, &Path, &codex_enrollment::Request),
) {
    let (_world, mut env, stage, request) = tests::fixture(true);
    let config = env.home.as_ref().unwrap().join("config");
    aterm_secrets::fsx::ensure_private_dir(&config).unwrap();
    let path = config.join("aterm.toml");
    std::fs::write(&path, "").unwrap();
    env.config = Some(path);
    std::fs::write(env.accounts_roster_for(super::accounts::Cli::Codex).unwrap(),
        format!("[[account]]\nlabel='fixture'\nemail='fixture@example.test'\nclass='dedicated'\ndir='{}'\n", stage.parent().unwrap().display())).unwrap();
    test(&env, &stage, &request);
}

#[cfg(test)]
#[path = "codex_initial_source_tests.rs"]
mod initial_source_tests;
