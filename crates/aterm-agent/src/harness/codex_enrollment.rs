// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Enrollment operation custody inside the account store's durable publication.
//! This module grants no provider, process, credential or session assignment authority.

use super::accounts::{self, Binding};
#[path = "codex_catalogue_witness.rs"]
mod catalogue_witness;
#[path = "codex_owner_witness.rs"]
mod owner_witness;
use aterm_json::Value;
pub(super) use catalogue_witness::{
    CatalogueAnchorWriter, CataloguePhase, CatalogueRecordState, CatalogueWitness,
};
pub(super) use owner_witness::{OwnerAnchorWriter, OwnerAttempt, OwnerWitness};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[path = "codex_enrollment_release.rs"]
pub(super) mod release;
pub use release::{RetirementControl, RetirementWork, reserve_cancelled_retirement};

const MAX_OPERATIONS: usize = 256;

/// An already source-resolved row and immutable operation identity. The native
/// session adapter supplies caller binding; callers never supply credential paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub caller: String,
    pub label: String,
    pub revision: String,
}

impl Request {
    pub fn intent(&self) -> RejectedIntent {
        RejectedIntent {
            id: self.id.clone(),
            caller: self.caller.clone(),
            account_id: account_id(&self.label),
            revision: self.revision.clone(),
        }
    }

    pub fn valid(&self) -> bool {
        hex(&self.id, 32)
            && hex(&self.caller, 64)
            && hex(&self.revision, 64)
            && accounts::admissible_label(&self.label)
    }
}

/// Opaque wire identity with no source-row authority. A rejected record stores
/// it only as Refused; admitted records project it for status correlation.
/// It never authorizes workflow, checkpoint, provider or credential effects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RejectedIntent {
    pub id: String,
    pub caller: String,
    pub account_id: String,
    pub revision: String,
}

impl RejectedIntent {
    pub fn valid(&self) -> bool {
        hex(&self.id, 32)
            && hex(&self.caller, 64)
            && hex(&self.account_id, 64)
            && hex(&self.revision, 64)
    }
}

/// An opaque reference is observation metadata, never a row or identity grant.
pub fn account_id(label: &str) -> String {
    let mut bytes = b"aterm-codex-pool-account-v1\0".to_vec();
    bytes.extend_from_slice(label.as_bytes());
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Public progress contains neither a credential nor its location. Running after
/// a restart is uncertain progress and cannot dispatch another provider attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Running,
    AuthenticationRequired,
    Refused,
    CancelRequested,
    Cancelled,
    Applied,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub phase: Phase,
    pub generation: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request: Option<Request>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rejected: Option<RejectedIntent>,
    #[serde(default, skip_serializing_if = "is_false")]
    native_required: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    catalogue_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grant_reference: Option<super::codex_grant_owner::GrantReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_witness: Option<OwnerWitness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    catalogue_witness: Option<CatalogueWitness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    row_publication: Option<super::codex_native_row::PublicationReceiptData>,
    status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    staging: Option<Staging>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    retired: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sealed_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovery: Option<RecoveryClaim>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryClaim {
    generation: u64,
    retired: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<RecoveryKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    occurrence: Option<NativeRecoveryOccurrence>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecoveryKind {
    NativePublication,
    NativeRetirement,
}
impl RecoveryKind {
    fn kernel_only(self) -> bool {
        matches!(self, Self::NativePublication | Self::NativeRetirement)
    }
}

/// Canonical occurrence data is not a proof constructor. The opaque ownership
/// receipt additionally binds the exact request/stage/ref and actual role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRecoveryOccurrence {
    nonce: String,
    reference: super::codex_grant_owner::GrantReference,
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct RecoveryOwnership {
    generation: u64,
    native: Option<NativeRecoveryOwnership>,
}
#[derive(Clone, PartialEq, Eq)]
struct NativeRecoveryOwnership {
    operation: super::account_work::OperationIdentity,
    occurrence: NativeRecoveryOccurrence,
    kind: RecoveryKind,
    request: Request,
    staging: Staging,
}
impl std::fmt::Debug for RecoveryOwnership {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryOwnership(redacted)")
    }
}
impl RecoveryOwnership {
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
    pub(super) fn nonce(&self) -> Option<&str> {
        self.native
            .as_ref()
            .map(|native| native.occurrence.nonce.as_str())
    }
    pub(super) fn belongs_to(&self, operation: &super::account_work::OperationIdentity) -> bool {
        self.native
            .as_ref()
            .is_some_and(|native| &native.operation == operation)
    }
    fn legacy(generation: u64) -> Self {
        Self {
            generation,
            native: None,
        }
    }
    fn native(
        generation: u64,
        kind: RecoveryKind,
        (request, staging, reference): (
            &Request,
            &Staging,
            &super::codex_grant_owner::GrantReference,
        ),
        nonce: &str,
        operation: super::account_work::OperationIdentity,
    ) -> Result<Self, String> {
        if !hex(nonce, 32) {
            return Err("native recovery occurrence unavailable".into());
        }
        let occurrence = NativeRecoveryOccurrence {
            nonce: nonce.to_owned(),
            reference: reference.clone(),
        };
        Ok(Self {
            generation,
            native: Some(NativeRecoveryOwnership {
                operation,
                occurrence,
                kind,
                request: request.clone(),
                staging: staging.clone(),
            }),
        })
    }
}
impl Record {
    fn catalogue_only_change(&self, old: &Self) -> bool {
        let mut unchanged = self.clone();
        unchanged.catalogue_witness = old.catalogue_witness.clone();
        unchanged == *old
    }

    fn native_retirement_claim_only(&self, old: &Self, owner: &RecoveryOwnership) -> bool {
        if old.status.phase != Phase::CancelRequested
            || self.status != old.status
            || !old.retired
            || self.recovery.as_ref().is_none_or(|claim| {
                claim.retired
                    || claim.kind != Some(RecoveryKind::NativeRetirement)
                    || !claim.matches(owner, self)
            })
        {
            return false;
        }
        // Include every current/future Record field: only the typed recovery
        // occurrence may change while cancellation remains pending.
        let mut unchanged = self.clone();
        unchanged.recovery = old.recovery.clone();
        unchanged == *old
    }
}

impl RecoveryClaim {
    fn matches(&self, owner: &RecoveryOwnership, record: &Record) -> bool {
        self.generation == owner.generation
            && match (&self.occurrence, &owner.native) {
                (None, None) => self.kind.is_none(),
                (Some(occurrence), Some(native)) => {
                    self.kind == Some(native.kind)
                        && occurrence == &native.occurrence
                        && record.request.as_ref() == Some(&native.request)
                        && record.staging.as_ref() == Some(&native.staging)
                        && record
                            .grant_reference
                            .as_ref()
                            .is_none_or(|reference| reference == &occurrence.reference)
                }
                _ => false,
            }
    }
    fn from_native(owner: &RecoveryOwnership) -> Self {
        let native = owner.native.as_ref().expect("native receipt factory");
        Self {
            generation: owner.generation,
            retired: false,
            kind: Some(native.kind),
            occurrence: Some(native.occurrence.clone()),
        }
    }
}

/// Immutable metadata of one durably admitted native attempt. No credential is kept here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Staging {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_row: Option<super::codex_native_row::NativeRowFence>,
    pub request: Request,
    pub store: std::path::PathBuf,
    pub directory: std::path::PathBuf,
    pub store_identity: Option<DirectoryIdentity>,
    pub directory_identity: Option<DirectoryIdentity>,
    pub parent: accounts::RenewalParent,
    pub workflow: u64,
    pub policy: String,
    pub selection: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectoryIdentity {
    dev: u64,
    ino: u64,
    volume: Option<[u8; 16]>,
}

impl DirectoryIdentity {
    pub(super) fn capture(path: &std::path::Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let identity = aterm_uds::directory_identity::read(path)
                .map_err(|_| "enrollment directory identity unavailable")?;
            Ok(Self {
                dev: identity.dev,
                ino: identity.ino,
                volume: identity.volume,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err("native enrollment directory identity unavailable on this platform".into())
        }
    }

    pub(super) fn matches(&self, path: &std::path::Path) -> bool {
        Self::capture(path).is_ok_and(|current| {
            self.ino == current.ino
                && match (self.volume, current.volume) {
                    (Some(left), Some(right)) => left == right,
                    _ => self.dev == current.dev,
                }
        })
    }
}

impl Staging {
    pub(super) fn store_current(&self) -> bool {
        self.store_identity
            .as_ref()
            .is_some_and(|identity| identity.matches(&self.store))
    }

    pub(super) fn directories_current(&self) -> bool {
        self.store_current()
            && self
                .directory_identity
                .as_ref()
                .is_some_and(|identity| identity.matches(&self.directory))
    }
}

/// Only the original signer/writer admission constructs this native receipt destination.
#[derive(Debug)]
pub struct Artifact {
    staging: Staging,
    deadline: std::sync::OnceLock<super::wake_deadline::WakeDeadline>,
}

pub(super) struct AppliedArtifact {
    admission: Staging,
    digest: String,
    generation: u64,
}

impl AppliedArtifact {
    pub(super) fn admission(&self) -> &Staging {
        &self.admission
    }
    pub(super) fn digest(&self) -> &str {
        &self.digest
    }
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
}

impl Artifact {
    pub(super) fn staging(&self) -> &Staging {
        &self.staging
    }

    pub(super) fn bind_deadline(
        &self,
        deadline: super::wake_deadline::WakeDeadline,
    ) -> Result<(), String> {
        if self.deadline.set(deadline).is_err() && self.deadline.get() != Some(&deadline) {
            return Err("enrollment producer deadline changed".into());
        }
        Ok(())
    }

    pub(super) fn producer_deadline(&self) -> Option<super::wake_deadline::WakeDeadline> {
        self.deadline.get().copied()
    }

    pub(super) fn work_current(&self) -> bool {
        self.deadline
            .get()
            .is_some_and(|deadline| !deadline.passed())
    }
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Record {
    fn intent(&self) -> Option<RejectedIntent> {
        match (&self.request, &self.rejected) {
            (Some(request), None) if request.valid() => Some(request.intent()),
            (None, Some(intent))
                if !self.native_required
                    && self.grant_reference.is_none()
                    && self.owner_witness.is_none()
                    && intent.valid()
                    && self.status
                        == (Status {
                            phase: Phase::Refused,
                            generation: None,
                        }) =>
            {
                Some(intent.clone())
            }
            _ => None,
        }
    }
}

/// Retained operation tombstones share logins.json custody. Capacity refuses
/// new operations; it never evicts an uncertain attempt or revives a spent ID.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Operations(
    BTreeMap<String, Record>,
    #[serde(skip)] BTreeMap<String, RetirementReceipt>,
    #[serde(skip)] std::collections::BTreeSet<String>,
    #[serde(skip)] std::collections::BTreeSet<String>,
    #[serde(skip)] BTreeMap<String, (Option<OwnerWitness>, OwnerWitness)>,
    #[serde(skip)] BTreeMap<String, RecoveryOwnership>,
    #[serde(skip)] std::collections::BTreeSet<String>,
    #[serde(skip)] BTreeMap<String, (Option<CatalogueWitness>, CatalogueWitness)>,
);

#[derive(Clone, Debug, PartialEq, Eq)]
enum RetirementReceipt {
    Original,
    Recovery(Box<RecoveryOwnership>),
    RecordedCancellation,
}

impl Operations {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn has_native_rows(&self) -> bool {
        self.0.values().any(|record| {
            record.row_publication.is_some()
                || record
                    .staging
                    .as_ref()
                    .is_some_and(|staging| staging.native_row.is_some())
        })
    }
    pub(super) fn require_complete_row_publications(&self) -> Result<(), String> {
        if self.0.values().any(|record| {
            record.status.phase == Phase::Applied
                && record
                    .staging
                    .as_ref()
                    .is_some_and(|staging| staging.native_row.is_some())
                && record.row_publication.is_none()
        }) {
            return Err("native row publication receipt is unavailable".into());
        }
        Ok(())
    }
    pub(super) fn attach_row_publication(
        &mut self,
        receipt: super::codex_native_row::RecordPublicationReceipt,
        bindings: &BTreeMap<String, Binding>,
    ) -> Result<(), String> {
        let data = receipt.into_data();
        let binding = data.binding();
        let record = self
            .0
            .get_mut(&binding.request.id)
            .ok_or("native row publication operation unavailable")?;
        if !data.valid()
            || record.row_publication.is_some()
            || !record.native_required
            || record.request.as_ref() != Some(&binding.request)
            || record.grant_reference.as_ref() != Some(&binding.reference)
            || record.staging.as_ref() != Some(data.staging())
            || record.status
                != (Status {
                    phase: Phase::Applied,
                    generation: Some(binding.generation),
                })
            || super::codex_native_row::binding_digest_for(
                &binding.request.label,
                bindings.get(&binding.request.label),
            )? != data.binding_fingerprint()
        {
            return Err("native row publication receipt changed".into());
        }
        let id = binding.request.id.clone();
        record.row_publication = Some(data);
        self.6.insert(id);
        Ok(())
    }
    /// Untrusted receipt data from canonical operations. The native row join
    /// authenticates each producer and publication MAC before using a delta.
    pub(super) fn row_publications_after(
        &self,
        clock: u64,
    ) -> Result<Vec<super::codex_native_row::PublicationReceiptData>, String> {
        let mut data = Vec::new();
        for record in self.0.values() {
            let Some(receipt) = &record.row_publication else {
                continue;
            };
            if receipt.binding().generation <= clock {
                continue;
            }
            if !self.authenticates_published_grant(receipt.binding()) {
                return Err("native row publication is not canonical".into());
            }
            data.push(receipt.clone());
        }
        data.sort_by_key(|receipt| receipt.binding().generation);
        Ok(data)
    }

    /// Canonical correlation data only; current credential use must verify the
    /// actual issuer MAC and the present after-binding fingerprint.
    pub(super) fn row_publication(
        &self,
        binding: &super::codex_grant_owner::GrantBinding,
    ) -> Option<&super::codex_native_row::PublicationReceiptData> {
        self.0
            .get(&binding.request.id)
            .filter(|_| self.authenticates_published_grant(binding))
            .and_then(|record| record.row_publication.as_ref())
            .filter(|data| data.binding() == binding)
    }

    pub(crate) fn has_catalogue_witness(&self) -> bool {
        self.0
            .values()
            .any(|record| record.catalogue_required || record.catalogue_witness.is_some())
    }

    pub(super) fn catalogue_required_id(&self, id: &str) -> bool {
        self.0
            .get(id)
            .is_some_and(|record| record.native_required && record.catalogue_required)
    }

    /// Dispatch metadata only; this cannot authorize a credential read.
    pub(super) fn has_native_label(&self, label: &str) -> bool {
        self.0.values().any(|record| {
            record.native_required
                && record
                    .request
                    .as_ref()
                    .is_some_and(|request| request.label == label)
        })
    }

    pub(super) fn requires_global_catalogue(&self, request: &Request) -> Result<bool, String> {
        self.lookup(request)?.ok_or("unknown catalogue operation")?;
        Ok(self
            .0
            .get(&request.id)
            .is_some_and(|record| record.native_required && record.catalogue_required))
    }

    pub(super) fn catalogue_witness(
        &self,
        request: &Request,
    ) -> Result<Option<CatalogueWitness>, String> {
        self.lookup(request)?.ok_or("unknown catalogue operation")?;
        let record = self
            .0
            .get(&request.id)
            .ok_or("unknown catalogue operation")?;
        if !record.native_required || !record.catalogue_required || record.staging.is_none() {
            return Err("catalogue source is not natively admitted".into());
        }
        Ok(record.catalogue_witness.clone())
    }
    pub(super) fn catalogue_staging(&self, request: &Request) -> Result<Staging, String> {
        self.catalogue_witness(request)?;
        self.0
            .get(&request.id)
            .and_then(|record| record.staging.clone())
            .ok_or_else(|| "catalogue staging unavailable".into())
    }
    pub(super) fn catalogue_record_state(
        &self,
        request: &Request,
    ) -> Result<CatalogueRecordState, String> {
        self.catalogue_staging(request)?;
        let record = self
            .0
            .get(&request.id)
            .ok_or("catalogue record unavailable")?;
        let published = if record.status.phase == Phase::Applied {
            let binding = super::codex_grant_owner::GrantBinding {
                request: request.clone(),
                reference: record
                    .grant_reference
                    .clone()
                    .ok_or("catalogue grant reference unavailable")?,
                generation: record
                    .status
                    .generation
                    .filter(|generation| *generation > 0)
                    .ok_or("catalogue publication generation unavailable")?,
            };
            if !self.authenticates_published_grant(&binding) {
                return Err("catalogue publication is uncertain".into());
            }
            Some(binding)
        } else {
            None
        };
        Ok(CatalogueRecordState {
            witness: record.catalogue_witness.clone(),
            reference: record.grant_reference.clone(),
            published,
            owner_witness: record.owner_witness.clone(),
            publication: record.row_publication.clone(),
        })
    }

    pub(super) fn cas_catalogue_witness(
        &mut self,
        transition: &super::codex_grant_owner::CatalogueTransition,
    ) -> Result<(), String> {
        let request = transition.request();
        let expected = transition.expected();
        let next = transition.next();
        if !next.valid()
            || self.catalogue_witness(request)?.as_ref() != expected
            || self.staged(request)? != Some(transition.staging())
            || !transition.valid()
        {
            return Err("catalogue anchor changed".into());
        }
        if expected == Some(next) {
            return Ok(());
        }
        self.0
            .get_mut(&request.id)
            .ok_or("unknown catalogue operation")?
            .catalogue_witness = Some(next.clone());
        self.7
            .insert(request.id.clone(), (expected.cloned(), next.clone()));
        Ok(())
    }

    pub(crate) fn has_owner_witness(&self) -> bool {
        self.0.values().any(|record| record.owner_witness.is_some())
    }

    pub(crate) fn requires_extended(&self) -> bool {
        self.0.values().any(|record| {
            record.rejected.is_some() || record.native_required || record.grant_reference.is_some()
        })
    }

    pub(crate) fn has_artifacts(&self) -> bool {
        self.0.values().any(|record| {
            record.staging.is_some() || record.retired || record.sealed_digest.is_some()
        })
    }

    pub(super) fn owns_staging_path(&self, path: &std::path::Path) -> bool {
        self.0.values().any(|record| {
            record
                .staging
                .as_ref()
                .is_some_and(|admission| admission.directory == path)
        })
    }

    pub(crate) fn encode(&self) -> Value {
        aterm_json::to_value(self).expect("bounded enrollment records serialize")
    }

    pub(crate) fn decode(value: &Value) -> Result<Self, String> {
        let book: Self = aterm_json::from_value(value.clone())
            .map_err(|_| "invalid enrollment custody".to_owned())?;
        if book.0.is_empty()
            || book.0.len() > MAX_OPERATIONS
            || book.0.iter().any(|(id, record)| {
                record.intent().is_none_or(|intent| id != &intent.id)
                    || (record.catalogue_required && !record.native_required)
                    || record.catalogue_witness.as_ref().is_some_and(|witness| {
                        !witness.valid()
                            || !record.native_required
                            || !record.catalogue_required
                            || record.staging.is_none()
                    })
                    || record.owner_witness.as_ref().is_some_and(|witness| {
                        !witness.valid()
                            || !record.native_required
                            || record.grant_reference.is_none()
                            || record.status.phase != Phase::Applied
                    })
                    || record
                        .grant_reference
                        .as_ref()
                        .is_some_and(|reference| !reference.valid())
                    || record.row_publication.as_ref().is_some_and(|data| {
                        !data.valid()
                            || !record.native_required
                            || record.status.phase != Phase::Applied
                            || record.status.generation != Some(data.binding().generation)
                            || record.request.as_ref() != Some(&data.binding().request)
                            || record.grant_reference.as_ref() != Some(&data.binding().reference)
                            || record.staging.as_ref() != Some(data.staging())
                    })
                    || record
                        .staging
                        .as_ref()
                        .and_then(|staging| staging.native_row.as_ref())
                        .is_some_and(|fence| {
                            !record.native_required
                                || !fence.valid()
                                || record
                                    .request
                                    .as_ref()
                                    .is_none_or(|request| request.label != fence.label())
                        })
                    || (record.grant_reference.is_some() && !record.native_required)
                    || (record.native_required
                        && record.status.phase == Phase::Applied
                        && record.grant_reference.is_none())
                    || record.recovery.as_ref().is_some_and(|claim| {
                        claim.generation == 0
                            || claim.kind.is_some() != claim.occurrence.is_some()
                            || claim.occurrence.as_ref().is_some_and(|occurrence| {
                                !hex(&occurrence.nonce, 32)
                                    || !occurrence.reference.valid()
                                    || record
                                        .grant_reference
                                        .as_ref()
                                        .is_some_and(|reference| reference != &occurrence.reference)
                            })
                            || !record.retired
                            || record.staging.is_none()
                            || (claim.kind.is_some_and(RecoveryKind::kernel_only)
                                && (!record.native_required
                                    || record
                                        .staging
                                        .as_ref()
                                        .is_none_or(|stage| stage.native_row.is_none())))
                    })
                    || record.recovery.as_ref().is_some_and(|claim| {
                        claim.kind == Some(RecoveryKind::NativeRetirement)
                            && !matches!(
                                record.status.phase,
                                Phase::CancelRequested | Phase::Cancelled
                            )
                    })
                    || match record.status.phase {
                        Phase::Applied => !record.status.generation.is_some_and(|n| n > 0),
                        _ => record.status.generation.is_some(),
                    }
            })
        {
            return Err("invalid enrollment custody".into());
        }
        book.require_complete_row_publications()?;
        Ok(book)
    }

    /// Metadata for operations owned by the exact native caller. This is not a
    /// transfer of ownership and does not grant provider or publication effects.
    pub fn retained_for(&self, caller: &str) -> Result<Vec<(RejectedIntent, Status)>, String> {
        if !hex(caller, 64) {
            return Err("invalid enrollment caller".into());
        }
        let mut retained = Vec::new();
        for record in self.0.values() {
            let intent = record.intent().ok_or("invalid enrollment custody")?;
            if intent.caller == caller {
                retained.push((intent, record.status.clone()));
            }
        }
        Ok(retained)
    }

    /// Resolve immutable wire metadata; neither record kind transfers ownership.
    pub fn retained(&self, id: &str, caller: &str) -> Result<(RejectedIntent, Status), String> {
        if !hex(id, 32) || !hex(caller, 64) {
            return Err("invalid enrollment operation".into());
        }
        self.0
            .get(id)
            .and_then(|record| {
                let intent = record.intent()?;
                (intent.caller == caller).then(|| (intent, record.status.clone()))
            })
            .ok_or_else(|| "unknown enrollment operation".into())
    }

    /// Recover only an existing row-resolved record. An opaque refusal cannot
    /// supply a row, workflow or checkpoint identity.
    pub fn admitted(&self, id: &str, caller: &str) -> Result<Request, String> {
        self.retained(id, caller)?;
        self.0
            .get(id)
            .and_then(|record| record.request.clone())
            .ok_or_else(|| "rejected intent has no admitted row".into())
    }

    pub(crate) fn lookup(&self, request: &Request) -> Result<Option<Status>, String> {
        if !request.valid() {
            return Err("invalid enrollment request".into());
        }
        self.0
            .get(&request.id)
            .map(|record| {
                if record.intent().as_ref() != Some(&request.intent())
                    || record
                        .request
                        .as_ref()
                        .is_some_and(|existing| existing != request)
                {
                    Err("enrollment operation ID was already used".into())
                } else {
                    Ok(record.status.clone())
                }
            })
            .transpose()
    }

    pub(crate) fn reject(&mut self, intent: &RejectedIntent) -> Result<Status, String> {
        if !intent.valid() {
            return Err("invalid enrollment intent".into());
        }
        if let Some(record) = self.0.get(&intent.id) {
            return if record.intent().as_ref() == Some(intent) {
                Ok(record.status.clone())
            } else {
                Err("enrollment operation ID was already used".into())
            };
        }
        if self.0.len() >= MAX_OPERATIONS {
            return Err("enrollment operation custody is full".into());
        }
        let status = Status {
            phase: Phase::Refused,
            generation: None,
        };
        self.0.insert(
            intent.id.clone(),
            Record {
                request: None,
                rejected: Some(intent.clone()),
                native_required: false,
                catalogue_required: false,
                grant_reference: None,
                owner_witness: None,
                catalogue_witness: None,
                row_publication: None,
                status: status.clone(),
                staging: None,
                retired: false,
                sealed_digest: None,
                recovery: None,
            },
        );
        self.2.insert(intent.id.clone());
        Ok(status)
    }

    pub(crate) fn admit(&mut self, request: &Request) -> Result<bool, String> {
        if self.lookup(request)?.is_some() {
            return Ok(false);
        }
        if self.0.len() >= MAX_OPERATIONS {
            return Err("enrollment operation custody is full".into());
        }
        if self.0.values().any(|record| {
            record
                .request
                .as_ref()
                .is_some_and(|existing| existing.label == request.label)
                && matches!(record.status.phase, Phase::Running | Phase::CancelRequested)
        }) {
            return Err("this account already has an uncertain enrollment operation".into());
        }
        self.0.insert(
            request.id.clone(),
            Record {
                request: Some(request.clone()),
                rejected: None,
                native_required: false,
                catalogue_required: false,
                grant_reference: None,
                owner_witness: None,
                catalogue_witness: None,
                row_publication: None,
                staging: None,
                retired: false,
                sealed_digest: None,
                recovery: None,
                status: Status {
                    phase: Phase::Running,
                    generation: None,
                },
            },
        );
        Ok(true)
    }

    pub(crate) fn admit_native(&mut self, request: &Request) -> Result<bool, String> {
        let fresh = self.admit(request)?;
        if fresh {
            let record = self.0.get_mut(&request.id).expect("admitted");
            record.native_required = true;
            record.catalogue_required = true;
        }
        Ok(fresh)
    }

    /// Historical-format fixture only. Production never upgrades/downgrades
    /// catalogue membership based on copied response bytes or file absence.
    #[cfg(all(test, unix))]
    pub(super) fn admit_historical_native_fixture(
        &mut self,
        request: &Request,
    ) -> Result<bool, String> {
        let fresh = self.admit(request)?;
        if fresh {
            self.0
                .get_mut(&request.id)
                .expect("admitted fixture")
                .native_required = true;
        }
        Ok(fresh)
    }

    pub(super) fn attach_grant(
        &mut self,
        request: &Request,
        grant: &super::codex_grant_owner::RetainedGrant,
    ) -> Result<(), String> {
        self.require_running(request)?;
        if !grant.matches_request(request) {
            return Err("retained grant belongs to another operation".into());
        }
        let reference = grant.reference();
        let record = self.0.get_mut(&request.id).expect("checked record");
        if !record.native_required {
            return Err("operation did not admit native ownership".into());
        }
        match &record.grant_reference {
            Some(existing) if existing == &reference => Ok(()),
            Some(_) => Err("operation already retained another grant".into()),
            None => {
                record.grant_reference = Some(reference);
                self.3.insert(request.id.clone());
                Ok(())
            }
        }
    }

    /// Ordering metadata only. Every candidate must be authenticated again
    /// under canonical owner custody before it can become a maintenance ticket.
    pub(super) fn global_owner_candidates_after(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Vec<super::codex_grant_owner::GrantBinding> {
        self.0
            .iter()
            .filter_map(|(id, record)| {
                if after.is_some_and(|after| id.as_str() <= after)
                    || !record.catalogue_required
                    || record.owner_witness.is_none()
                {
                    return None;
                }
                let binding = super::codex_grant_owner::GrantBinding {
                    request: record.request.clone()?,
                    reference: record.grant_reference.clone()?,
                    generation: record.status.generation?,
                };
                self.authenticates_published_grant(&binding)
                    .then_some(binding)
            })
            .take(limit)
            .collect()
    }

    pub(super) fn authenticates_published_grant(
        &self,
        binding: &super::codex_grant_owner::GrantBinding,
    ) -> bool {
        binding.generation > 0
            && self.0.get(&binding.request.id).is_some_and(|record| {
                record.request.as_ref() == Some(&binding.request)
                    && record.native_required
                    && record.grant_reference.as_ref() == Some(&binding.reference)
                    && (!record
                        .staging
                        .as_ref()
                        .is_some_and(|staging| staging.native_row.is_some())
                        || record.row_publication.is_some())
                    && record.status
                        == (Status {
                            phase: Phase::Applied,
                            generation: Some(binding.generation),
                        })
            })
    }

    /// Canonical correlation only; owner admission still authenticates issuer.
    pub(super) fn published_grant(
        &self,
        label: &str,
        actual_success_generation: u64,
    ) -> Option<super::codex_grant_owner::GrantBinding> {
        self.find_published_grant(label, actual_success_generation, false)
    }
    /// Only the genuine pre-image permit can inspect its own provisional view.
    /// This never authorizes canonical readers to accept a missing receipt.
    pub(super) fn provisional_published_for_permit(
        &self,
        label: &str,
        generation: u64,
        permit: &accounts::ActualPublicationPermit<'_>,
    ) -> Option<super::codex_grant_owner::GrantBinding> {
        permit
            .owns_operations(self)
            .then(|| self.find_published_grant(label, generation, true))
            .flatten()
    }
    pub(super) fn provisional_owner_for_permit(
        &self,
        binding: &super::codex_grant_owner::GrantBinding,
        permit: &accounts::ActualPublicationPermit<'_>,
    ) -> Result<Option<OwnerWitness>, String> {
        if !permit.owns_operations(self)
            || self
                .find_published_grant(&binding.request.label, binding.generation, true)
                .as_ref()
                != Some(binding)
        {
            return Err("native publication is not this derived view".into());
        }
        Ok(self
            .0
            .get(&binding.request.id)
            .ok_or("native publication unavailable")?
            .owner_witness
            .clone())
    }
    fn find_published_grant(
        &self,
        label: &str,
        actual_success_generation: u64,
        provisional: bool,
    ) -> Option<super::codex_grant_owner::GrantBinding> {
        if actual_success_generation == 0 {
            return None;
        }
        let mut matches = self.0.values().filter_map(|record| {
            let request = record.request.as_ref()?;
            let reference = record.grant_reference.as_ref()?;
            (request.label == label
                && record.native_required
                && (provisional
                    || !record
                        .staging
                        .as_ref()
                        .is_some_and(|staging| staging.native_row.is_some())
                    || record.row_publication.is_some())
                && reference.valid()
                && record.status
                    == (Status {
                        phase: Phase::Applied,
                        generation: Some(actual_success_generation),
                    }))
            .then(|| super::codex_grant_owner::GrantBinding {
                request: request.clone(),
                reference: reference.clone(),
                generation: actual_success_generation,
            })
        });
        let unique = matches.next()?;
        matches.next().is_none().then_some(unique)
    }

    pub(super) fn owner_witness(
        &self,
        binding: &super::codex_grant_owner::GrantBinding,
    ) -> Result<Option<OwnerWitness>, String> {
        if !self.authenticates_published_grant(binding) {
            return Err("grant is not canonically published".into());
        }
        Ok(self
            .0
            .get(&binding.request.id)
            .expect("authenticated record")
            .owner_witness
            .clone())
    }

    pub(super) fn cas_owner_witness(
        &mut self,
        transition: &super::codex_grant_owner::OwnerWitnessTransition,
    ) -> Result<(), String> {
        let binding = transition.binding();
        let expected = transition.expected();
        let next = transition.next();
        if !next.valid() || self.owner_witness(binding)?.as_ref() != expected {
            return Err("owner witness changed".into());
        }
        match expected {
            None if transition.is_fresh_activation()
                && next.revision.generation == 1
                && next.pending.is_none() => {}
            Some(old)
                if !transition.is_fresh_activation()
                    && next.revision.generation >= old.revision.generation
                    && (next.revision.generation != old.revision.generation
                        || next.revision.digest == old.revision.digest)
                    && (next.revision.generation == old.revision.generation
                        || (old.pending.is_some() && next.pending.is_none()))
                    && !(old.pending.is_some()
                        && next.pending.is_some()
                        && old.pending != next.pending) => {}
            _ => return Err("invalid owner witness transition".into()),
        }
        if expected == Some(next) {
            return Ok(());
        }
        let previous = expected.cloned();
        self.0
            .get_mut(&binding.request.id)
            .expect("authenticated record")
            .owner_witness = Some(next.clone());
        self.4
            .insert(binding.request.id.clone(), (previous, next.clone()));
        Ok(())
    }

    pub(crate) fn require_running(&self, request: &Request) -> Result<(), String> {
        match self.lookup(request)? {
            Some(Status {
                phase: Phase::Running,
                ..
            }) => Ok(()),
            _ => Err("enrollment operation is no longer running".into()),
        }
    }

    fn refuse_rejected(
        &mut self,
        rejection: &super::codex_grant_owner::RetiredRejection,
    ) -> Result<(), String> {
        let request = rejection.request();
        let status = self
            .lookup(request)?
            .ok_or("enrollment operation unavailable")?;
        if status.phase != Phase::Running {
            return Ok(());
        }
        let record = self.0.get(&request.id).expect("checked request");
        // Historical outcome evidence cannot retire a newer recovery owner.
        if record.recovery.as_ref().is_some_and(|claim| !claim.retired) {
            return Ok(());
        }
        if !record.native_required
            || record.grant_reference.is_some()
            || record.owner_witness.is_some()
        {
            return Err("provider rejection conflicts with retained credentials".into());
        }
        self.finish(request, Phase::Refused)
    }

    pub(crate) fn finish(&mut self, request: &Request, phase: Phase) -> Result<(), String> {
        self.require_running(request)?;
        if matches!(phase, Phase::Applied | Phase::Cancelled) {
            return Err("enrollment needs committed publication or retirement evidence".into());
        }
        self.0
            .get_mut(&request.id)
            .expect("checked record")
            .status
            .phase = phase;
        Ok(())
    }

    /// Transaction-local marker; the account generation guard fills the actual
    /// committed success generation and refuses a marker without a Renewed event.
    pub(super) fn checkpoint_reference(
        &self,
        request: &Request,
    ) -> Result<Option<super::codex_grant_owner::GrantReference>, String> {
        self.require_running(request)?;
        let record = self.0.get(&request.id).expect("checked record");
        if record.native_required && record.grant_reference.is_none() {
            return Err("native enrollment has no retained grant".into());
        }
        Ok(record.grant_reference.clone())
    }

    pub(super) fn requires_native_grant(&self, request: &Request) -> Result<bool, String> {
        self.require_running(request)?;
        Ok(self
            .0
            .get(&request.id)
            .expect("checked record")
            .native_required)
    }

    pub(crate) fn publishing(&mut self, request: &Request) -> Result<(), String> {
        self.require_running(request)?;
        if self
            .0
            .get(&request.id)
            .is_some_and(|record| record.native_required && record.grant_reference.is_none())
        {
            return Err("native enrollment has no retained grant authority".into());
        }
        self.0
            .get_mut(&request.id)
            .expect("checked record")
            .status
            .phase = Phase::Applied;
        Ok(())
    }

    pub(crate) fn retire(&mut self, proof: &Retired) -> Result<(), String> {
        let status = self
            .lookup(&proof.request)?
            .ok_or("unknown enrollment operation")?;
        if !matches!(status.phase, Phase::Running | Phase::CancelRequested) {
            return Ok(());
        }
        let record = self.0.get(&proof.request.id).expect("checked record");
        if let RetiredKind::Recovery(owner) = &proof.kind
            && record
                .recovery
                .as_ref()
                .is_none_or(|claim| !claim.matches(owner, record))
        {
            return Err("enrollment recovery occurrence changed".into());
        }
        let record = self.0.get_mut(&proof.request.id).expect("checked record");
        match &proof.kind {
            RetiredKind::Original => {
                if record.recovery.as_ref().is_some_and(|claim| !claim.retired) {
                    return Err("a newer recovery still owns account work".into());
                }
                if record.retired {
                    return self.retire_recorded_cancellation(&proof.request);
                }
                record.retired = true;
            }
            RetiredKind::Recovery(owner) => {
                let claim = record
                    .recovery
                    .as_mut()
                    .filter(|claim| claim.generation == owner.generation)
                    .ok_or("enrollment recovery claim changed")?;
                if claim.retired {
                    return self.retire_recorded_cancellation(&proof.request);
                }
                claim.retired = true;
            }
        }
        if self
            .lookup(&proof.request)?
            .is_some_and(|status| status.phase == Phase::Running)
        {
            self.0
                .get_mut(&proof.request.id)
                .expect("checked record")
                .retired = true;
            self.1
                .insert(proof.request.id.clone(), proof.kind.receipt());
            return Ok(());
        }
        match self.lookup(&proof.request)? {
            Some(Status {
                phase: Phase::Cancelled,
                generation: None,
            }) => return Ok(()),
            Some(Status {
                phase: Phase::CancelRequested,
                generation: None,
            }) => {}
            _ => return Err("enrollment cancellation is not awaiting retirement".into()),
        }
        self.0
            .get_mut(&proof.request.id)
            .expect("checked record")
            .status
            .phase = Phase::Cancelled;
        self.1
            .insert(proof.request.id.clone(), proof.kind.receipt());
        Ok(())
    }

    pub(super) fn bind_staging(&mut self, staging: Staging) -> Result<Artifact, String> {
        self.require_running(&staging.request)?;
        let record = self.0.get_mut(&staging.request.id).expect("checked record");
        if staging
            .native_row
            .as_ref()
            .is_some_and(|fence| !record.native_required || !fence.valid())
        {
            return Err("native row admission is unavailable".into());
        }
        if record.staging.is_some() || record.retired {
            return Err("enrollment already owns an attempt".into());
        }
        record.staging = Some(staging.clone());
        Ok(Artifact {
            staging,
            deadline: std::sync::OnceLock::new(),
        })
    }

    pub(super) fn staged(&self, request: &Request) -> Result<Option<&Staging>, String> {
        self.lookup(request)?
            .ok_or("unknown enrollment operation")?;
        Ok(self
            .0
            .get(&request.id)
            .and_then(|record| record.staging.as_ref()))
    }

    pub(super) fn is_retired(&self, request: &Request) -> Result<bool, String> {
        self.lookup(request)?
            .ok_or("unknown enrollment operation")?;
        Ok(self.0.get(&request.id).is_some_and(|record| {
            record.retired && record.recovery.as_ref().is_none_or(|claim| claim.retired)
        }))
    }

    pub(super) fn claim_recovery(&mut self, request: &Request) -> Result<u64, String> {
        self.require_running(request)?;
        if !self.is_retired(request)? || self.staged(request)?.is_none() {
            return Err("enrollment recovery is not retired".into());
        }
        let generation = self
            .0
            .get(&request.id)
            .and_then(|record| record.recovery.as_ref())
            .map_or(0, |claim| claim.generation)
            .checked_add(1)
            .ok_or("enrollment recovery generation exhausted")?;
        self.0
            .get_mut(&request.id)
            .expect("checked record")
            .recovery = Some(RecoveryClaim {
            generation,
            retired: false,
            kind: None,
            occurrence: None,
        });
        self.5
            .insert(request.id.clone(), RecoveryOwnership::legacy(generation));
        Ok(generation)
    }

    /// The original attempt must actually have retired. An active legacy or
    /// unknown recovery remains unavailable; only our kernel-only role can be
    /// superseded after reacquiring all of its real exclusive resources.
    pub(super) fn require_native_recovery_candidate(
        &self,
        request: &Request,
    ) -> Result<(), String> {
        self.require_running(request)?;
        let record = self
            .0
            .get(&request.id)
            .ok_or("unknown enrollment operation")?;
        if !record.retired
            || !record.native_required
            || record
                .staging
                .as_ref()
                .is_none_or(|stage| stage.native_row.is_none())
            || record.recovery.as_ref().is_some_and(|claim| {
                !claim.retired && claim.kind != Some(RecoveryKind::NativePublication)
            })
        {
            return Err("native recovery original resources are not retired".into());
        }
        Ok(())
    }

    /// A reference lookup for the already retired native recovery role only.
    /// Absence permits producer-sealed pending recovery solely before any
    /// canonical recovery claim exists; it never weakens the legacy getter.
    pub(super) fn native_recovery_reference(
        &self,
        request: &Request,
    ) -> Result<Option<super::codex_grant_owner::GrantReference>, String> {
        self.require_native_recovery_candidate(request)?;
        let record = self
            .0
            .get(&request.id)
            .ok_or("unknown enrollment operation")?;
        if let Some(reference) = &record.grant_reference {
            return Ok(Some(reference.clone()));
        }
        match &record.recovery {
            None => Ok(None),
            Some(claim) if claim.kind == Some(RecoveryKind::NativePublication) => claim
                .occurrence
                .as_ref()
                .map(|occurrence| Some(occurrence.reference.clone()))
                .ok_or_else(|| "native recovery occurrence unavailable".into()),
            Some(_) => Err("native recovery has no authenticated reference".into()),
        }
    }

    pub(super) fn claim_native_publication_recovery(
        &mut self,
        resources: &super::native_enrollment_admission::NativeRecoveryResources,
    ) -> Result<RecoveryOwnership, String> {
        resources.current()?;
        let candidate = resources.candidate();
        let request = candidate.request();
        self.require_native_recovery_candidate(request)?;
        let record = self
            .0
            .get(&request.id)
            .ok_or("unknown enrollment operation")?;
        if record.staging.as_ref() != Some(candidate.staging())
            || record
                .grant_reference
                .as_ref()
                .is_some_and(|reference| reference != candidate.reference())
        {
            return Err("native recovery occurrence changed".into());
        }
        let generation = record
            .recovery
            .as_ref()
            .map_or(0, |claim| claim.generation)
            .checked_add(1)
            .ok_or("enrollment recovery generation exhausted")?;
        let owner = RecoveryOwnership::native(
            generation,
            RecoveryKind::NativePublication,
            (
                candidate.request(),
                candidate.staging(),
                candidate.reference(),
            ),
            resources.occurrence(),
            resources.operation_identity(),
        )?;
        self.0
            .get_mut(&request.id)
            .expect("checked record")
            .recovery = Some(RecoveryClaim::from_native(&owner));
        self.5.insert(request.id.clone(), owner.clone());
        Ok(owner)
    }

    pub(super) fn require_native_retirement_candidate(
        &self,
        request: &Request,
    ) -> Result<(), String> {
        let status = self
            .lookup(request)?
            .ok_or("unknown enrollment operation")?;
        let record = self
            .0
            .get(&request.id)
            .ok_or("unknown enrollment operation")?;
        if status.phase != Phase::CancelRequested
            || !record.retired
            || !record.native_required
            || record
                .staging
                .as_ref()
                .is_none_or(|stage| stage.native_row.is_none())
            || record.recovery.as_ref().is_none_or(|claim| {
                claim.retired || !claim.kind.is_some_and(RecoveryKind::kernel_only)
            })
        {
            return Err("native retirement has no cancelled publication claim".into());
        }
        Ok(())
    }

    pub(super) fn native_retirement_reference(
        &self,
        request: &Request,
    ) -> Result<Option<super::codex_grant_owner::GrantReference>, String> {
        self.require_native_retirement_candidate(request)?;
        let record = self
            .0
            .get(&request.id)
            .ok_or("unknown enrollment operation")?;
        let occurrence = record
            .recovery
            .as_ref()
            .and_then(|claim| claim.occurrence.as_ref())
            .ok_or("native retirement occurrence unavailable")?;
        if record
            .grant_reference
            .as_ref()
            .is_some_and(|reference| reference != &occurrence.reference)
        {
            return Err("native retirement reference changed".into());
        }
        Ok(Some(
            record
                .grant_reference
                .as_ref()
                .unwrap_or(&occurrence.reference)
                .clone(),
        ))
    }

    pub(super) fn claim_native_retirement_recovery(
        &mut self,
        resources: &super::native_enrollment_admission::NativeRetirementResources,
    ) -> Result<RecoveryOwnership, String> {
        resources.current()?;
        let candidate = resources.candidate();
        let request = candidate.request();
        self.require_native_retirement_candidate(request)?;
        let record = self
            .0
            .get(&request.id)
            .ok_or("unknown enrollment operation")?;
        if record.staging.as_ref() != Some(candidate.staging())
            || record
                .grant_reference
                .as_ref()
                .is_some_and(|reference| reference != candidate.reference())
        {
            return Err("native retirement occurrence changed".into());
        }
        let generation = record
            .recovery
            .as_ref()
            .map_or(0, |claim| claim.generation)
            .checked_add(1)
            .ok_or("enrollment recovery generation exhausted")?;
        let owner = RecoveryOwnership::native(
            generation,
            RecoveryKind::NativeRetirement,
            (
                candidate.request(),
                candidate.staging(),
                candidate.reference(),
            ),
            resources.occurrence(),
            resources.operation_identity(),
        )?;
        self.0
            .get_mut(&request.id)
            .expect("checked record")
            .recovery = Some(RecoveryClaim::from_native(&owner));
        self.5.insert(request.id.clone(), owner.clone());
        Ok(owner)
    }

    pub(super) fn require_native_publication_recovery(
        &self,
        request: &Request,
        owner: &RecoveryOwnership,
    ) -> Result<(), String> {
        self.require_running(request)?;
        let record = self
            .0
            .get(&request.id)
            .ok_or("unknown enrollment operation")?;
        if record.recovery.as_ref().is_none_or(|claim| {
            claim.retired
                || claim.kind != Some(RecoveryKind::NativePublication)
                || !claim.matches(owner, record)
        }) {
            return Err("native recovery occurrence changed".into());
        }
        Ok(())
    }

    pub(super) fn require_recovery(
        &self,
        request: &Request,
        generation: u64,
    ) -> Result<(), String> {
        self.require_running(request)?;
        if self.0.get(&request.id).is_some_and(|record| {
            record.retired
                && record
                    .recovery
                    .as_ref()
                    .is_some_and(|claim| claim.generation == generation && !claim.retired)
        }) {
            Ok(())
        } else {
            Err("enrollment recovery claim changed".into())
        }
    }

    /// An exact retired native attempt may be revalidated locally under a fresh admission.
    /// This is metadata only; it authorizes neither a provider request nor publication.
    pub fn recoverable(&self, request: &Request) -> Result<bool, String> {
        Ok(self
            .lookup(request)?
            .is_some_and(|status| status.phase == Phase::Running)
            && self.is_retired(request)?
            && self.staged(request)?.is_some())
    }

    /// Exact retained request metadata, including terminal status replay. This
    /// confers no credential, mutation, resource, or retirement authority.
    pub fn native_admitted(&self, request: &Request) -> Result<bool, String> {
        self.lookup(request)?
            .ok_or("unknown enrollment operation")?;
        Ok(self
            .0
            .get(&request.id)
            .expect("checked request")
            .native_required)
    }

    /// Cancellation dispatch metadata only, including a native operation whose
    /// stage is not yet bound. This confers no resource or grant authority.
    pub fn native_cancellation(&self, request: &Request) -> Result<bool, String> {
        let Some(status) = self.lookup(request)? else {
            return Ok(false);
        };
        Ok(self.native_admitted(request)?
            && matches!(status.phase, Phase::Running | Phase::CancelRequested))
    }

    /// Dispatch observation only. It permits no credential or canonical write;
    /// the worker must acquire real native resources and authenticate its row.
    pub fn native_recovery_pending(&self, request: &Request) -> Result<bool, String> {
        let Some(status) = self.lookup(request)? else {
            return Ok(false);
        };
        Ok(match status.phase {
            Phase::Running => self.require_native_recovery_candidate(request).is_ok(),
            Phase::CancelRequested => self.require_native_retirement_candidate(request).is_ok(),
            _ => false,
        })
    }

    pub(super) fn retire_recorded_cancellation(&mut self, request: &Request) -> Result<(), String> {
        if self
            .lookup(request)?
            .is_some_and(|status| status.phase == Phase::CancelRequested)
            && self.is_retired(request)?
        {
            self.0
                .get_mut(&request.id)
                .expect("checked record")
                .status
                .phase = Phase::Cancelled;
            self.1
                .insert(request.id.clone(), RetirementReceipt::RecordedCancellation);
        }
        Ok(())
    }

    pub(super) fn applied_artifact(
        &self,
        home: &std::path::Path,
        bindings: &BTreeMap<String, Binding>,
    ) -> Option<AppliedArtifact> {
        self.0.values().find_map(|record| {
            let admission = record.staging.as_ref()?;
            let generation = record.status.generation?;
            let binding = bindings.get(&admission.request.label)?;
            if record.status.phase != Phase::Applied
                || generation == 0
                || admission.directory != home
                || binding.last_success_generation < generation
                || (binding.dir.as_deref() != home.to_str()
                    && binding.last_success_generation == generation)
            {
                return None;
            }
            Some(AppliedArtifact {
                admission: admission.clone(),
                digest: record.sealed_digest.clone()?,
                generation,
            })
        })
    }

    pub(super) fn seal_publication(
        &mut self,
        request: &Request,
        digest: String,
    ) -> Result<(), String> {
        self.require_running(request)?;
        if !hex(&digest, 64) {
            return Err("invalid enrollment digest".into());
        }
        let record = self.0.get_mut(&request.id).expect("checked record");
        if record
            .sealed_digest
            .as_ref()
            .is_some_and(|previous| previous != &digest)
        {
            return Err("enrollment payload changed".into());
        }
        record.sealed_digest = Some(digest);
        Ok(())
    }

    pub(crate) fn commit(
        &mut self,
        before: &Self,
        renewed: &std::collections::BTreeSet<String>,
        bindings: &BTreeMap<String, Binding>,
    ) -> Result<(), String> {
        if self.0.len() > MAX_OPERATIONS
            || before.0.iter().any(|(id, old)| {
                self.0.get(id).is_none_or(|new| {
                    new.request != old.request
                        || new.rejected != old.rejected
                        || new.native_required != old.native_required
                        || new.catalogue_required != old.catalogue_required
                        || (new.grant_reference != old.grant_reference
                            && !(old.status.phase == Phase::Running
                                && old.grant_reference.is_none()
                                && new.grant_reference.is_some()
                                && new.native_required
                                && self.3.contains(id)))
                        || (new.row_publication != old.row_publication
                            && !(old.row_publication.is_none() && self.6.contains(id)))
                        || (new.catalogue_witness != old.catalogue_witness
                            && !self.7.get(id).is_some_and(|(expected, next)| {
                                expected == &old.catalogue_witness
                                    && new.catalogue_witness.as_ref() == Some(next)
                                    && new.catalogue_only_change(old)
                            }))
                        || (new.owner_witness != old.owner_witness
                            && !self.4.get(id).is_some_and(|(expected, next)| {
                                expected == &old.owner_witness
                                    && new.owner_witness.as_ref() == Some(next)
                                    && old.status.phase == Phase::Applied
                                    && new.status == old.status
                            }))
                        || (old.staging.is_some() && new.staging != old.staging)
                        || (old.sealed_digest.is_some() && new.sealed_digest != old.sealed_digest)
                        || (old.retired && !new.retired)
                        || (old.retired != new.retired
                            && self.1.get(id) != Some(&RetirementReceipt::Original))
                        || (old.staging.is_none()
                            && new.staging.is_some()
                            && old.status.phase != Phase::Running)
                        || (old.status.phase != Phase::Running
                            && new != old
                            && !self.4.contains_key(id)
                            && !self.6.contains(id)
                            && !(self.7.contains_key(id) && new.catalogue_only_change(old))
                            && !self
                                .5
                                .get(id)
                                .is_some_and(|owner| new.native_retirement_claim_only(old, owner))
                            && !(old.status.phase == Phase::CancelRequested
                                && new.status.phase == Phase::Cancelled
                                && new.status.generation.is_none()
                                && self.1.contains_key(id)))
                })
            })
        {
            return Err("enrollment operation custody was erased or altered".into());
        }
        for (id, receipt) in &self.1 {
            if !before.0.get(id).is_some_and(|old| {
                matches!(old.status.phase, Phase::CancelRequested | Phase::Running)
                    && match receipt {
                        RetirementReceipt::Original => {
                            old.recovery.as_ref().is_none_or(|claim| claim.retired)
                        }
                        RetirementReceipt::Recovery(owner) => {
                            old.retired
                                && old.recovery.as_ref().is_some_and(|claim| {
                                    claim.matches(owner, old) && !claim.retired
                                })
                        }
                        RetirementReceipt::RecordedCancellation => {
                            old.retired && old.recovery.as_ref().is_none_or(|claim| claim.retired)
                        }
                    }
            }) || !self.0.get(id).is_some_and(|new| {
                new.status.phase == Phase::Cancelled
                    || (new.status.phase == Phase::Running && new.retired)
            }) {
                return Err("invalid enrollment retirement evidence".into());
            }
        }
        for (id, record) in &mut self.0 {
            if record.intent().is_none_or(|intent| id != &intent.id) {
                return Err("invalid enrollment operation".into());
            }
            if let Some(staging) = &record.staging
                && (record.request.as_ref() != Some(&staging.request)
                    || !staging.store.is_absolute()
                    || !staging.directory.is_absolute()
                    || staging.workflow == 0
                    || !hex(&staging.policy, 64))
            {
                return Err("invalid enrollment staging admission".into());
            }
            let old = before.0.get(id);
            if let Some(old) = old
                && old.recovery != record.recovery
            {
                let claimed = record.recovery.as_ref().is_some_and(|claim| {
                    !claim.retired
                        && self
                            .5
                            .get(id)
                            .is_some_and(|owner| claim.matches(owner, record))
                        && old.retired
                        && ((old.status.phase == Phase::Running
                            && claim.kind != Some(RecoveryKind::NativeRetirement)
                            && old.recovery.as_ref().is_none_or(|previous| {
                                previous.retired
                                    || (previous.kind == Some(RecoveryKind::NativePublication)
                                        && claim.kind == Some(RecoveryKind::NativePublication))
                            }))
                            || (old.status.phase == Phase::CancelRequested
                                && claim.kind == Some(RecoveryKind::NativeRetirement)
                                && old.recovery.as_ref().is_some_and(|previous| {
                                    previous.kind.is_some_and(RecoveryKind::kernel_only)
                                })))
                        && (claim.kind.is_none()
                            || (record.native_required
                                && record
                                    .staging
                                    .as_ref()
                                    .is_some_and(|stage| stage.native_row.is_some())))
                        && old
                            .recovery
                            .as_ref()
                            .map_or(0, |previous| previous.generation)
                            .checked_add(1)
                            == Some(claim.generation)
                });
                let retired = old
                    .recovery
                    .as_ref()
                    .zip(record.recovery.as_ref())
                    .is_some_and(|(previous, claim)| {
                        previous.generation == claim.generation
                            && previous.kind == claim.kind
                            && previous.occurrence == claim.occurrence
                            && !previous.retired
                            && claim.retired
                            && self.1.get(id).is_some_and(|receipt| match receipt {
                                RetirementReceipt::Recovery(owner) => claim.matches(owner, record),
                                _ => false,
                            })
                    });
                if !claimed && !retired {
                    return Err("enrollment recovery has no ownership receipt".into());
                }
            }
            if record.status.phase == Phase::Cancelled
                && !old.is_some_and(|old| old.status.phase == Phase::Cancelled)
                && !self.1.contains_key(id)
            {
                return Err("enrollment cancellation has no retirement evidence".into());
            }
            if old.is_none() {
                if record.retired
                    || record.staging.is_some()
                    || record.sealed_digest.is_some()
                    || record.recovery.is_some()
                {
                    return Err("new enrollment has no retired native attempt".into());
                }
                let new_row = record.request.is_some()
                    && record.rejected.is_none()
                    && record.grant_reference.is_none()
                    && record.owner_witness.is_none()
                    && record.row_publication.is_none()
                    && record.status
                        == (Status {
                            phase: Phase::Running,
                            generation: None,
                        });
                let new_refusal = record.request.is_none()
                    && record.rejected.is_some()
                    && self.2.contains(id)
                    && record.status
                        == (Status {
                            phase: Phase::Refused,
                            generation: None,
                        });
                if !new_row && !new_refusal {
                    return Err("new enrollment has no admission or refusal evidence".into());
                }
            }
            if old.is_some_and(|old| old.status.phase == Phase::Running)
                && record.status.phase == Phase::Applied
            {
                if record.native_required && record.grant_reference.is_none() {
                    return Err("native publication has no retained grant reference".into());
                }
                let request = record
                    .request
                    .as_ref()
                    .ok_or("rejected intent cannot publish")?;
                if record.status.generation.is_some() || !renewed.contains(&request.label) {
                    return Err("enrollment publication has no admitted renewal".into());
                }
                record.status.generation = bindings
                    .get(&request.label)
                    .map(|binding| binding.last_success_generation)
                    .filter(|n| *n > 0);
                if record.status.generation.is_none() {
                    return Err("enrollment has no committed generation".into());
                }
            } else if record.status.phase != Phase::Applied && record.status.generation.is_some() {
                return Err("unpublished enrollment has a generation".into());
            }
        }
        for id in &self.2 {
            if before.0.contains_key(id)
                || !self
                    .0
                    .get(id)
                    .is_some_and(|record| record.rejected.is_some())
            {
                return Err("invalid no-action refusal evidence".into());
            }
        }
        for id in &self.3 {
            if !before.0.get(id).is_some_and(|old| {
                old.status.phase == Phase::Running
                    && old.native_required
                    && old.grant_reference.is_none()
            }) || !self
                .0
                .get(id)
                .is_some_and(|record| record.grant_reference.is_some() && record.native_required)
            {
                return Err("invalid retained grant attachment evidence".into());
            }
        }
        for (id, (expected, next)) in &self.4 {
            if !next.valid()
                || !before.0.get(id).is_some_and(|old| {
                    old.status.phase == Phase::Applied
                        && old.native_required
                        && old.grant_reference.is_some()
                        && &old.owner_witness == expected
                })
                || !self.0.get(id).is_some_and(|new| {
                    new.owner_witness.as_ref() == Some(next) && new.status.phase == Phase::Applied
                })
            {
                return Err("invalid owner witness mutation evidence".into());
            }
        }
        for id in &self.6 {
            if !before.0.get(id).is_some_and(|old| {
                old.native_required
                    && old.status.phase == Phase::Applied
                    && old.row_publication.is_none()
            }) || !self.0.get(id).is_some_and(|new| {
                new.row_publication
                    .as_ref()
                    .is_some_and(|data| data.valid() && data.binding().request.id == *id)
            }) {
                return Err("native row publication has no transaction evidence".into());
            }
        }
        for (id, (expected, next)) in &self.7 {
            if !before.0.get(id).is_some_and(|old| {
                old.native_required && old.staging.is_some() && &old.catalogue_witness == expected
            }) || !self.0.get(id).is_some_and(|new| {
                new.catalogue_witness.as_ref() == Some(next)
                    && before
                        .0
                        .get(id)
                        .is_some_and(|old| new.catalogue_only_change(old))
            }) {
                return Err("invalid catalogue witness mutation evidence".into());
            }
        }
        self.7.clear();
        self.6.clear();
        self.4.clear();
        self.5.clear();
        self.1.clear();
        self.2.clear();
        self.3.clear();
        Ok(())
    }
}

#[cfg(all(unix, any(test, debug_assertions)))]
#[path = "codex_enrollment_pending_fixture.rs"]
mod pending_fixture;
#[cfg(all(unix, any(test, debug_assertions)))]
pub use pending_fixture::{PendingCustodyFixture, PendingCustodyRestore};

#[derive(Default)]
struct NativeRuntime {
    pending: std::sync::Mutex<
        Option<(
            Box<super::codex_grant_owner::RetentionFailure>,
            super::account_work::Scope,
        )>,
    >,
    reference: std::sync::Mutex<Option<super::codex_grant_owner::GrantReference>>,
}

pub(super) struct NativeContext<'a> {
    pub(super) env: &'a super::cli::Env,
    runtime: &'a std::sync::Arc<NativeRuntime>,
    pub(super) scope: &'a super::account_work::Scope,
    pub(super) retirement: &'a std::sync::Mutex<Option<super::account_work::ReleaseTurn>>,
}

impl NativeRuntime {
    fn capture(
        &self,
        outcome: super::codex_grant_owner::CaptureOutcome,
        scope: &super::account_work::Scope,
    ) -> Option<super::codex_grant_owner::RetainedGrant> {
        use super::codex_grant_owner::CaptureOutcome;
        match outcome {
            CaptureOutcome::Verified(grant) => {
                *self.reference.lock().unwrap_or_else(|e| e.into_inner()) = Some(grant.reference());
                Some(grant)
            }
            CaptureOutcome::Unverified(_retained) => None,
            CaptureOutcome::NotDispatched(_receipt) => None,
            CaptureOutcome::Rejected(_receipt) => None,
            CaptureOutcome::Pending(failure) => {
                *self.pending.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((failure, scope.clone()));
                None
            }
        }
    }
}

impl NativeContext<'_> {
    pub(super) fn capture(
        &self,
        outcome: super::codex_grant_owner::CaptureOutcome,
    ) -> Option<super::codex_grant_owner::RetainedGrant> {
        self.runtime.capture(outcome, self.scope)
    }
}

/// Work owner minted before dispatch; only a successful durable new admission
/// enables its request-bound retirement proof.
pub struct Work {
    native: std::sync::Arc<NativeRuntime>,
    request: Request,
    scope: super::account_work::Scope,
    admitted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    recovery: std::sync::Arc<std::sync::Mutex<Option<RecoveryOwnership>>>,
}

/// Native adapter handle retaining original work while completed-response custody is pending.
pub struct Control {
    operation: super::account_work::OperationIdentity,
    native: std::sync::Arc<NativeRuntime>,
    request: Request,
    cancellation: super::account_work::Cancellation,
    admitted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    recovery: std::sync::Arc<std::sync::Mutex<Option<RecoveryOwnership>>>,
}

/// Evidence of retirement for the exact durably admitted request. Fields are
/// private; a newly minted scope for a replayed ID cannot construct this proof.
pub struct Retired {
    request: Request,
    kind: RetiredKind,
}

enum RetiredKind {
    Original,
    Recovery(Box<RecoveryOwnership>),
}

impl RetiredKind {
    fn receipt(&self) -> RetirementReceipt {
        match self {
            Self::Original => RetirementReceipt::Original,
            Self::Recovery(owner) => RetirementReceipt::Recovery(owner.clone()),
        }
    }
}

pub(super) struct Admission<'a> {
    scope: &'a super::account_work::Scope,
    request: &'a Request,
    original: &'a std::sync::atomic::AtomicBool,
    recovery: &'a std::sync::Mutex<Option<RecoveryOwnership>>,
}

impl Admission<'_> {
    pub(super) fn original(&self) {
        self.original
            .store(true, std::sync::atomic::Ordering::Release);
    }
    pub(super) fn recovery(&self, generation: u64) -> Result<(), String> {
        let mut owned = self
            .recovery
            .lock()
            .map_err(|_| "enrollment recovery owner unavailable")?;
        if owned.is_some() {
            return Err("enrollment work already owns a recovery".into());
        }
        *owned = Some(RecoveryOwnership::legacy(generation));
        Ok(())
    }
    pub(super) fn native_recovery(&self, owner: &RecoveryOwnership) -> Result<(), String> {
        if owner.native.as_ref().is_none_or(|native| {
            &native.request != self.request || !native.operation.matches_scope(self.scope)
        }) {
            return Err("native recovery has no matching occurrence".into());
        }
        let mut owned = self
            .recovery
            .lock()
            .map_err(|_| "enrollment recovery owner unavailable")?;
        if owned.is_some() {
            return Err("enrollment work already owns a recovery".into());
        }
        *owned = Some(owner.clone());
        Ok(())
    }
}

fn retired_for(
    request: &Request,
    original: &std::sync::atomic::AtomicBool,
    recovery: &std::sync::Mutex<Option<RecoveryOwnership>>,
    operation: &super::account_work::OperationIdentity,
    retired: bool,
) -> Option<Retired> {
    if !retired {
        return None;
    }
    let kind = if original.load(std::sync::atomic::Ordering::Acquire) {
        RetiredKind::Original
    } else {
        let owner = recovery.lock().ok()?.as_ref()?.clone();
        if owner.native.is_some() && !owner.belongs_to(operation) {
            return None;
        }
        RetiredKind::Recovery(Box::new(owner))
    };
    Some(Retired {
        request: request.clone(),
        kind,
    })
}

impl Retired {
    pub(super) fn request(&self) -> &Request {
        &self.request
    }

    pub fn finish(self, env: &super::cli::Env) -> Result<Status, String> {
        super::cli::retire_codex_operation(env, &self)
    }
}

pub fn owned_work(request: Request, scope: super::account_work::Scope) -> (Work, Control) {
    let admitted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let native = std::sync::Arc::new(NativeRuntime::default());
    let recovery = std::sync::Arc::new(std::sync::Mutex::new(None));
    let control = Control {
        operation: scope.operation_identity(),
        native: native.clone(),
        request: request.clone(),
        cancellation: scope.cancellation(),
        admitted: admitted.clone(),
        recovery: recovery.clone(),
    };
    (
        Work {
            native,
            request,
            scope,
            admitted,
            recovery,
        },
        control,
    )
}

impl Work {
    /// Inert provider injection for native account-management controls.
    #[cfg(any(test, debug_assertions))]
    pub fn run_with_host(
        self,
        env: &super::cli::Env,
        host: &dyn super::cli::AccountsHost,
    ) -> Result<Status, String> {
        let cancellation = self.scope.cancellation();
        let operation = self.scope.operation_identity();
        let admission = Admission {
            scope: &self.scope,
            request: &self.request,
            original: &self.admitted,
            recovery: &self.recovery,
        };
        let result =
            super::cli::enroll_codex_operation_admitted(env, host, &self.request, Some(&admission));
        drop(self.scope);
        if result
            .as_ref()
            .is_ok_and(|status| matches!(status.phase, Phase::Running | Phase::CancelRequested))
            && let Some(proof) = retired_for(
                &self.request,
                &self.admitted,
                &self.recovery,
                &operation,
                cancellation.retired(),
            )
        {
            return proof.finish(env);
        }
        result
    }

    /// Test host injection through the actual NativeContext/Admission doorway.
    /// It consumes an existing counted Work; it creates no grant or Scope.
    #[cfg(all(test, unix))]
    pub(in crate::harness) fn run_native_with_host(
        self,
        env: &super::cli::Env,
        host: &dyn super::cli::AccountsHost,
    ) -> Result<Status, String> {
        let cancellation = self.scope.cancellation();
        let operation = self.scope.operation_identity();
        let admission = Admission {
            scope: &self.scope,
            request: &self.request,
            original: &self.admitted,
            recovery: &self.recovery,
        };
        let retirement = std::sync::Mutex::new(None);
        let context = NativeContext {
            env,
            runtime: &self.native,
            scope: &self.scope,
            retirement: &retirement,
        };
        let result = super::cli::enroll_codex_operation_custody(
            env,
            host,
            &self.request,
            Some(&admission),
            Some(&context),
        );
        let _retirement = retirement.into_inner().unwrap();
        drop(self.scope);
        if result
            .as_ref()
            .is_ok_and(|status| matches!(status.phase, Phase::Running | Phase::CancelRequested))
            && let Some(proof) = retired_for(
                &self.request,
                &self.admitted,
                &self.recovery,
                &operation,
                cancellation.retired(),
            )
        {
            return proof.finish(env);
        }
        result
    }

    #[cfg(all(test, unix))]
    pub(in crate::harness) fn fixture_capture_after_native_admission(
        &self,
        env: &super::cli::Env,
        outcome: super::codex_grant_owner::CaptureOutcome,
    ) {
        let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
        let (logins, warning) = store.load_logins(env.now).unwrap();
        assert!(warning.is_none());
        let record = logins.enrollment.0.get(&self.request.id).unwrap();
        assert!(
            record.request.as_ref() == Some(&self.request)
                && record.native_required
                && record.status.phase == Phase::Running
                && record.grant_reference.is_none()
        );
        self.admitted
            .store(true, std::sync::atomic::Ordering::Release);
        let _ = self.native.capture(outcome, &self.scope);
    }

    pub fn run(self, env: &super::cli::Env) -> Result<Status, String> {
        let cancellation = self.scope.cancellation();
        let operation = self.scope.operation_identity();
        let host = super::cli::LiveAccounts::manual_work(self.scope.clone());
        let admission = Admission {
            scope: &self.scope,
            request: &self.request,
            original: &self.admitted,
            recovery: &self.recovery,
        };
        let retirement = std::sync::Mutex::new(None);
        let result = {
            let context = NativeContext {
                env,
                runtime: &self.native,
                scope: &self.scope,
                retirement: &retirement,
            };
            super::cli::enroll_codex_operation_custody(
                env,
                &host,
                &self.request,
                Some(&admission),
                Some(&context),
            )
        };
        let _retirement = retirement
            .into_inner()
            .unwrap_or_else(|error| error.into_inner());
        drop(host);
        drop(self.scope);
        if result
            .as_ref()
            .is_ok_and(|status| matches!(status.phase, Phase::Running | Phase::CancelRequested))
            && let Some(proof) = retired_for(
                &self.request,
                &self.admitted,
                &self.recovery,
                &operation,
                cancellation.retired(),
            )
        {
            return proof.finish(env);
        }
        result
    }
}

/// Signal-worker reconciliation of authenticated no-send evidence. Read-only
/// discovery never calls this, and an unknown Attempted record is never proof.
pub fn settle_not_dispatched(env: &super::cli::Env, request: &Request) -> Result<(), String> {
    let Some(receipt) = super::codex_grant_owner::recover_not_dispatched(env, request)? else {
        return Ok(());
    };
    if !receipt.matches_request(request) {
        return Err("no-dispatch request changed".into());
    }
    let store = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("account store unavailable")?;
    store.transact_logins(env.now, &mut None, |_, logins| {
        let status = logins
            .enrollment
            .lookup(request)?
            .ok_or("enrollment operation unavailable")?;
        if status.phase == Phase::Running
            && logins
                .enrollment
                .0
                .get(&request.id)
                .is_some_and(|record| record.recovery.as_ref().is_none_or(|claim| claim.retired))
        {
            logins.enrollment.finish(request, Phase::Refused)?;
        }
        Ok(Vec::new())
    })?;
    Ok(())
}

fn finish_rejected(
    env: &super::cli::Env,
    rejection: &super::codex_grant_owner::RetiredRejection,
) -> Result<(), String> {
    let store = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("account store unavailable")?;
    store.transact_logins(env.now, &mut None, |_, logins| {
        logins.enrollment.refuse_rejected(rejection)?;
        Ok(Vec::new())
    })?;
    Ok(())
}

fn settle_rejected(env: &super::cli::Env, retired: &Retired) -> Result<(), String> {
    let Some(rejection) = super::codex_grant_owner::retain_rejection_retirement(env, retired)?
    else {
        return Ok(());
    };
    finish_rejected(env, &rejection)
}

/// Signal-worker recovery of a provider rejection whose exact owned resources
/// were already proved retired before host loss. This never infers retirement.
pub fn settle_retired_rejection(env: &super::cli::Env, request: &Request) -> Result<(), String> {
    let Some(rejection) = super::codex_grant_owner::recover_rejection_retirement(env, request)?
    else {
        return Ok(());
    };
    finish_rejected(env, &rejection)
}

impl Control {
    /// A custody observation only; this grants no publication or retirement authority.
    pub fn has_pending_custody(&self) -> bool {
        self.native
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Retry only encryption/publication of an already completed response.
    /// No provider request is replayed and no canonical account is adopted here.
    pub fn settle_custody(&self, env: &super::cli::Env) -> Result<(), String> {
        let pending = self
            .native
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some((failure, scope)) = pending {
            // The returned carrier still owns the completed response on failure;
            // capture restores that carrier and the original scope before drop.
            let _ = self.native.capture(failure.retry(), &scope);
        }
        // An owned retry still holds the grant journal claim. Do not reopen
        // that claim, or prevent cancellation from revoking publication while
        // storage settlement retains the original work scope.
        if self
            .native
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
        {
            let store = env
                .accounts_store_for(accounts::Cli::Codex)
                .ok_or("account store unavailable")?;
            let (logins, warning) = store.load_logins(env.now)?;
            if warning.is_some() {
                return Err("account state unavailable".into());
            }
            // These proofs can only refuse Running. They are not prerequisites
            // for an already requested cancellation: actual Scope retirement
            // must remain usable even if a persisted outcome becomes unreadable.
            if logins
                .enrollment
                .lookup(&self.request)?
                .is_some_and(|status| status.phase == Phase::Running)
            {
                settle_not_dispatched(env, &self.request)?;
                if let Some(retired) = self.retired() {
                    settle_rejected(env, &retired)?;
                }
            }
        }
        let reference = self
            .native
            .reference
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(reference) = reference {
            let store = env
                .accounts_store_for(accounts::Cli::Codex)
                .ok_or("account store unavailable")?;
            store.transact_logins(env.now, &mut None, |_, logins| {
                let status = logins
                    .enrollment
                    .lookup(&self.request)?
                    .ok_or("enrollment operation unavailable")?;
                if status.phase == Phase::Running {
                    let grant = super::codex_grant_owner::recover(env, &self.request, &reference)?;
                    logins.enrollment.attach_grant(&self.request, &grant)?;
                }
                Ok(Vec::new())
            })?;
        }
        Ok(())
    }

    /// Whether this invocation actually committed a new original or recovery admission.
    /// This observation is not a retirement or publication grant.
    pub fn owns_admitted_work(&self) -> bool {
        self.admitted.load(std::sync::atomic::Ordering::Acquire)
            || self.recovery.lock().is_ok_and(|owned| owned.is_some())
    }
    pub fn owned_by(&self, lifetime: &super::account_work::Lifetime) -> bool {
        self.cancellation.owned_by(lifetime)
    }
    /// Revoke durable publication before requesting provider cancellation.
    pub fn cancel(&self, env: &super::cli::Env) -> Result<Status, String> {
        let status = cancel(env, &self.request)?;
        if status.phase == Phase::CancelRequested {
            self.cancellation.cancel();
        }
        // Publication is revoked and the actual worker observes cancellation
        // even if storage settlement fails. Pending still owns its work scope.
        self.settle_custody(env)?;
        Ok(status)
    }

    /// Work-lifetime observation for bounded runtime cleanup, not a publication
    /// or cancellation-completion grant. Only `retired` carries that authority.
    pub fn work_retired(&self) -> bool {
        self.cancellation.retired()
    }

    pub fn retired(&self) -> Option<Retired> {
        retired_for(
            &self.request,
            &self.admitted,
            &self.recovery,
            &self.operation,
            self.cancellation.retired(),
        )
    }
}

pub(super) fn retain_grant(
    env: &super::cli::Env,
    request: &Request,
    grant: &super::codex_grant_owner::RetainedGrant,
) -> Result<(), String> {
    let store = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("account store unavailable")?;
    store.transact_logins(env.now, &mut None, |_, logins| {
        logins.enrollment.attach_grant(request, grant)?;
        Ok(Vec::new())
    })?;
    Ok(())
}

/// Persist a no-provider-action refusal in the same durable account writer.
/// The authenticated native owner supplies the actual caller and wire intent.
pub fn reject(env: &super::cli::Env, intent: &RejectedIntent) -> Result<Status, String> {
    let store = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("account store unavailable")?;
    let mut status = None;
    store.transact_logins(env.now, &mut None, |_, logins| {
        status = Some(logins.enrollment.reject(intent)?);
        Ok(Vec::new())
    })?;
    status.ok_or_else(|| "enrollment refusal was not committed".into())
}

/// Revoke this operation's publication authority. Owned process retirement is
/// supplied by its account_work scope; cancellation does not release that lease.
pub fn cancel(env: &super::cli::Env, request: &Request) -> Result<Status, String> {
    super::cli::cancel_codex_operation(env, request)
}

#[cfg(all(unix, any(test, debug_assertions)))]
#[path = "codex_enrollment_row_fixture.rs"]
mod row_fixture;
#[cfg(all(unix, any(test, debug_assertions)))]
pub use row_fixture::NativeRowRecoveryFixture;
#[cfg(all(unix, any(test, debug_assertions)))]
#[path = "codex_enrollment_source_proof_fixture.rs"]
mod source_proof_fixture;
#[cfg(all(unix, any(test, debug_assertions)))]
pub use source_proof_fixture::{
    NativeSourceProofWorld, SOURCE_PROOF_TASK, SourceProofAdvance, SourceProofLaunch,
    SourceProofSettlement,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_cancel_dispatch_metadata_covers_prestage_but_grants_no_recovery() {
        let request = request();
        let mut native = Operations::default();
        native.admit_native(&request).unwrap();
        assert!(native.native_cancellation(&request).unwrap());
        assert!(!native.native_recovery_pending(&request).unwrap());
        let mut foreign = request.clone();
        foreign.caller = "f".repeat(64);
        assert!(native.native_cancellation(&foreign).is_err());
        let mut legacy = Operations::default();
        legacy.admit(&request).unwrap();
        assert!(!legacy.native_cancellation(&request).unwrap());
    }

    #[test]
    fn new_or_replayed_metadata_cannot_mint_retired_work() {
        let empty = Operations::default();
        let mut forged = empty.clone();
        let request = request();
        forged.admit(&request).unwrap();
        forged.0.get_mut(&request.id).unwrap().retired = true;
        assert!(
            forged
                .commit(&empty, &Default::default(), &Default::default())
                .is_err()
        );
        let mut running = empty.clone();
        running.admit(&request).unwrap();
        running
            .commit(&empty, &Default::default(), &Default::default())
            .unwrap();
        let mut changed = running.clone();
        changed.0.get_mut(&request.id).unwrap().retired = true;
        assert!(
            changed
                .commit(&running, &Default::default(), &Default::default())
                .is_err()
        );
        let lifetime = super::super::account_work::Lifetime::default();
        let (work, control) = owned_work(request, lifetime.enter().unwrap());
        drop(work);
        assert!(
            control.retired().is_none(),
            "a fresh scope is not the original admitted work"
        );
    }

    #[test]
    fn original_and_stale_retirement_cannot_retire_a_new_recovery_claim() {
        let request = request();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut book = Operations::default();
        let before = book.clone();
        book.admit(&request).unwrap();
        book.commit(&before, &Default::default(), &Default::default())
            .unwrap();
        let before = book.clone();
        book.bind_staging(Staging {
            native_row: None,
            request: request.clone(),
            store: root.path().to_path_buf(),
            directory: root.path().join("staging"),
            store_identity: None,
            directory_identity: None,
            parent: accounts::RenewalParent::capture(&accounts::Logins::default(), &request.label),
            workflow: 1,
            policy: "d".repeat(64),
            selection: None,
        })
        .unwrap();
        book.commit(&before, &Default::default(), &Default::default())
            .unwrap();
        let lifetime = super::super::account_work::Lifetime::default();
        let (original, control) = owned_work(request.clone(), lifetime.enter().unwrap());
        original
            .admitted
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(control.retired().is_none());
        drop(original);
        let original = control.retired().unwrap();
        let before = book.clone();
        book.retire(&original).unwrap();
        book.commit(&before, &Default::default(), &Default::default())
            .unwrap();
        let before = book.clone();
        let generation = book.claim_recovery(&request).unwrap();
        book.commit(&before, &Default::default(), &Default::default())
            .unwrap();
        let active = book.clone();
        assert!(book.retire(&original).is_err());
        assert_eq!(book, active);
        let scope = lifetime.enter().unwrap();
        let detached = scope.clone();
        let (recovery, control) = owned_work(request.clone(), scope);
        let admission = Admission {
            scope: &recovery.scope,
            request: &recovery.request,
            original: &recovery.admitted,
            recovery: &recovery.recovery,
        };
        admission.recovery(generation).unwrap();
        drop(recovery);
        assert!(
            control.retired().is_none(),
            "detached original recovery resources remain owned"
        );
        drop(detached);
        let proof = control.retired().unwrap();
        let before = book.clone();
        book.retire(&proof).unwrap();
        book.commit(&before, &Default::default(), &Default::default())
            .unwrap();
        let mut restored = Operations::decode(&book.encode()).unwrap();
        assert!(restored.recoverable(&request).unwrap());
        let before = restored.clone();
        let second = restored.claim_recovery(&request).unwrap();
        assert_eq!(second, generation + 1);
        restored
            .commit(&before, &Default::default(), &Default::default())
            .unwrap();
        let active = restored.clone();
        assert!(restored.retire(&proof).is_err());
        assert_eq!(restored, active);
        // A serialized edit cannot launder an old original or recovery proof
        // into retirement of the currently live claim.
        for stale in [&original, &proof] {
            let mut forged = Operations::decode(&restored.encode()).unwrap();
            forged
                .0
                .get_mut(&request.id)
                .unwrap()
                .recovery
                .as_mut()
                .unwrap()
                .retired = true;
            let _ = forged.retire(stale);
            assert!(
                forged
                    .commit(&restored, &Default::default(), &Default::default())
                    .is_err()
            );
        }
        let (second_work, second_control) = owned_work(request.clone(), lifetime.enter().unwrap());
        Admission {
            scope: &second_work.scope,
            request: &second_work.request,
            original: &second_work.admitted,
            recovery: &second_work.recovery,
        }
        .recovery(second)
        .unwrap();
        drop(second_work);
        let second_proof = second_control.retired().unwrap();
        let before = restored.clone();
        restored.retire(&second_proof).unwrap();
        restored
            .commit(&before, &Default::default(), &Default::default())
            .unwrap();
        let before = restored.clone();
        let third = restored.claim_recovery(&request).unwrap();
        assert_eq!(third, second + 1);
        restored
            .commit(&before, &Default::default(), &Default::default())
            .unwrap();
        let active = restored.clone();
        assert!(restored.retire(&proof).is_err());
        assert!(restored.retire(&second_proof).is_err());
        assert_eq!(restored, active);
        let mut exhausted = restored;
        exhausted.0.get_mut(&request.id).unwrap().recovery = Some(RecoveryClaim {
            generation: u64::MAX,
            retired: true,
            kind: None,
            occurrence: None,
        });
        let exhausted = Operations::decode(&exhausted.encode()).unwrap();
        let mut attempted = exhausted.clone();
        assert!(attempted.claim_recovery(&request).is_err());
        assert_eq!(attempted, exhausted);
    }

    fn request() -> Request {
        Request {
            id: "a".repeat(32),
            caller: "b".repeat(64),
            label: "work".into(),
            revision: "c".repeat(64),
        }
    }

    #[test]
    fn native_admission_requires_authenticated_receipt_before_publication() {
        let request = request();
        let empty = Operations::default();
        let mut running = empty.clone();
        assert!(running.admit_native(&request).unwrap());
        running
            .commit(&empty, &Default::default(), &Default::default())
            .unwrap();
        assert!(running.requires_extended());
        assert!(running.checkpoint_reference(&request).is_err());
        assert!(running.clone().publishing(&request).is_err());
        let mut downgraded = running.clone();
        downgraded.0.get_mut(&request.id).unwrap().native_required = false;
        assert!(
            downgraded
                .commit(&running, &Default::default(), &Default::default())
                .is_err()
        );
        // Deserialization is correlation data, never a live retained capability.
        let reference = aterm_json::from_value(aterm_json::json!({
            "operation": request.id, "capability": "d".repeat(64),
            "issuance_digest": "e".repeat(64), "producer_mac": "f".repeat(64)
        }))
        .unwrap();
        let mut forged = running.clone();
        forged.0.get_mut(&request.id).unwrap().grant_reference = Some(reference);
        assert!(
            forged
                .commit(&running, &Default::default(), &Default::default())
                .is_err()
        );
        assert!(
            forged
                .commit(&empty, &Default::default(), &Default::default())
                .is_err()
        );
        assert!(forged.published_grant(&request.label, 9).is_none());
        forged.0.get_mut(&request.id).unwrap().status = Status {
            phase: Phase::Applied,
            generation: Some(9),
        };
        let binding = forged.published_grant(&request.label, 9).unwrap();
        assert_eq!(binding.request, request);
        assert_eq!(binding.generation, 9);
        assert!(forged.published_grant(&request.label, 0).is_none());
        assert!(forged.published_grant(&request.label, 8).is_none());
        assert!(forged.published_grant("other", 9).is_none());
        let mut duplicate = forged.0.get(&request.id).unwrap().clone();
        let duplicate_id = "e".repeat(32);
        duplicate.request.as_mut().unwrap().id = duplicate_id.clone();
        forged.0.insert(duplicate_id, duplicate);
        assert!(
            forged.published_grant(&request.label, 9).is_none(),
            "ambiguous published receipt is not selected"
        );
        let mut legacy = Operations::default();
        legacy.admit(&request).unwrap();
        assert!(!legacy.admit_native(&request).unwrap());
        assert!(
            !legacy.requires_extended(),
            "replay cannot upgrade ownership mode"
        );
    }

    #[test]
    fn owner_witness_requires_canonical_schema_and_private_mutation_evidence() {
        let request = request();
        let mut before = Operations::default();
        before.admit_native(&request).unwrap();
        // Synthetic correlation data supplies no encrypted owner authority.
        let reference = aterm_json::from_value(aterm_json::json!({
            "operation":request.id, "capability":"d".repeat(64), "issuance_digest":"e".repeat(64), "producer_mac":"f".repeat(64)
        })).unwrap();
        let record = before.0.get_mut(&request.id).unwrap();
        // This control exercises the historical schema-nine owner boundary,
        // before new native admissions required a global catalogue witness.
        record.catalogue_required = false;
        record.grant_reference = Some(reference);
        record.status = Status {
            phase: Phase::Applied,
            generation: Some(9),
        };
        let binding = before.published_grant(&request.label, 9).unwrap();
        assert!(before.owner_witness(&binding).unwrap().is_none());
        let witness = OwnerWitness {
            revision: super::super::codex_session_slot::Revision {
                generation: 1,
                digest: "1".repeat(64),
            },
            journal_digest: "2".repeat(64),
            pending: None,
        };
        let mut forged = before.clone();
        forged.0.get_mut(&request.id).unwrap().owner_witness = Some(witness.clone());
        assert!(
            forged
                .commit(&before, &Default::default(), &Default::default())
                .is_err()
        );
        let mut logins = accounts::Logins {
            enrollment: forged,
            ..Default::default()
        };
        let text = logins.to_json();
        let mut doc: Value = aterm_json::from_str(&text).unwrap();
        assert_eq!(doc["schema"].as_u64(), Some(9));
        assert!(
            !text.contains("\"rejected\""),
            "old witness bytes stay exact"
        );
        let decoded = accounts::Logins::from_json(&text, 1_800_000_000).unwrap();
        assert_eq!(
            decoded.enrollment.owner_witness(&binding).unwrap(),
            Some(witness.clone())
        );
        doc.as_object_mut()
            .unwrap()
            .insert("schema".into(), Value::from(8));
        assert!(accounts::Logins::from_json(&doc.to_string(), 1_800_000_000).is_err());
        let mut wrong = binding.clone();
        wrong.generation = 8;
        assert!(decoded.enrollment.owner_witness(&wrong).is_err());
        logins
            .enrollment
            .0
            .get_mut(&request.id)
            .unwrap()
            .owner_witness
            .as_mut()
            .unwrap()
            .pending = Some(OwnerAttempt {
            operation: "3".repeat(32),
            request_digest: "4".repeat(64),
            base_generation: 2,
            base_digest: witness.revision.digest.clone(),
            token_digest: "5".repeat(64),
        });
        assert!(
            accounts::Logins::from_json(&logins.to_json(), 1_800_000_000).is_err(),
            "pending attempt must match canonical current revision"
        );
        assert!(!format!("{witness:?}").contains(&"2".repeat(64)));
    }

    #[test]
    fn cancellation_commit_requires_exact_retirement_and_keeps_tombstone() {
        let request = request();
        let mut running = Operations::default();
        running.admit(&request).unwrap();
        let mut forged = running.clone();
        forged.0.get_mut(&request.id).unwrap().status.phase = Phase::Cancelled;
        assert!(
            forged
                .commit(&running, &Default::default(), &Default::default())
                .is_err()
        );
        let mut settled = running.clone();
        settled
            .retire(&Retired {
                request: request.clone(),
                kind: RetiredKind::Original,
            })
            .unwrap();
        settled
            .commit(&running, &Default::default(), &Default::default())
            .unwrap();
        assert!(settled.is_retired(&request).unwrap());
        assert_eq!(
            settled.lookup(&request).unwrap().unwrap().phase,
            Phase::Running
        );
        let mut pending = running.clone();
        pending.finish(&request, Phase::CancelRequested).unwrap();
        pending
            .commit(&running, &Default::default(), &Default::default())
            .unwrap();
        let mut forged = pending.clone();
        forged.0.get_mut(&request.id).unwrap().status.phase = Phase::Cancelled;
        assert!(
            forged
                .commit(&pending, &Default::default(), &Default::default())
                .is_err()
        );
        let mut retired = pending.clone();
        let mut wrong = request.clone();
        wrong.caller = "f".repeat(64);
        assert!(
            retired
                .retire(&Retired {
                    request: wrong,
                    kind: RetiredKind::Original
                })
                .is_err()
        );
        retired
            .retire(&Retired {
                request: request.clone(),
                kind: RetiredKind::Original,
            })
            .unwrap();
        retired
            .commit(&pending, &Default::default(), &Default::default())
            .unwrap();
        assert!(retired.1.is_empty());
        let mut restored = Operations::decode(&retired.encode()).unwrap();
        assert_eq!(
            restored.lookup(&request).unwrap().unwrap().phase,
            Phase::Cancelled
        );
        assert!(!restored.admit(&request).unwrap());
        let before = restored.clone();
        restored
            .retire(&Retired {
                request: request.clone(),
                kind: RetiredKind::Original,
            })
            .unwrap();
        assert!(
            restored.1.is_empty(),
            "repeat finalization supplies no new transition marker"
        );
        restored
            .commit(&before, &Default::default(), &Default::default())
            .unwrap();
        assert_eq!(restored, before);
        let mut wrong = request;
        wrong.caller = "f".repeat(64);
        assert!(
            restored
                .retire(&Retired {
                    request: wrong,
                    kind: RetiredKind::Original
                })
                .is_err()
        );
    }

    #[test]
    fn unadmitted_or_still_owned_work_cannot_prove_retirement() {
        let lifetime = super::super::account_work::Lifetime::default();
        let (work, control) = owned_work(request(), lifetime.enter().unwrap());
        assert!(control.retired().is_none());
        drop(work);
        assert!(control.retired().is_none(), "a replay cannot mint a permit");
        let (work, control) = owned_work(request(), lifetime.enter().unwrap());
        work.admitted
            .store(true, std::sync::atomic::Ordering::Release);
        let detached = work.scope.clone();
        drop(work);
        assert!(control.retired().is_none());
        drop(detached);
        assert_eq!(control.retired().unwrap().request, request());
    }

    #[test]
    fn durable_custody_refuses_replay_retarget_and_uncertain_restart() {
        let request = request();
        let mut book = Operations::default();
        assert!(book.admit(&request).unwrap());
        let mut restored = Operations::decode(&book.encode()).unwrap();
        assert!(!restored.admit(&request).unwrap());
        let mut changed = request.clone();
        changed.label = "other".into();
        assert!(restored.admit(&changed).is_err());
        changed = request.clone();
        changed.id = "d".repeat(32);
        assert!(restored.admit(&changed).is_err());
        assert!(restored.finish(&request, Phase::Cancelled).is_err());
        restored.finish(&request, Phase::CancelRequested).unwrap();
        assert!(restored.require_running(&request).is_err());
        assert!(
            restored.admit(&changed).is_err(),
            "unretired cancellation keeps custody"
        );
    }

    #[test]
    fn publication_needs_an_admitted_renewal_and_transaction_derived_generation() {
        let request = request();
        let mut before = Operations::default();
        before.admit(&request).unwrap();
        let mut after = before.clone();
        after.publishing(&request).unwrap();
        let mut bindings = BTreeMap::new();
        bindings.insert(
            "work".into(),
            Binding {
                last_success_generation: 9,
                ..Binding::default()
            },
        );
        assert!(
            after
                .clone()
                .commit(&before, &Default::default(), &bindings)
                .is_err()
        );
        let renewed = ["work".into()].into_iter().collect();
        let mut forged = after.clone();
        forged.0.get_mut(&request.id).unwrap().status.generation = Some(9);
        assert!(forged.commit(&before, &renewed, &bindings).is_err());
        assert!(
            after
                .clone()
                .commit(&Operations::default(), &renewed, &bindings)
                .is_err()
        );
        after.commit(&before, &renewed, &bindings).unwrap();
        assert_eq!(
            after.lookup(&request).unwrap().unwrap(),
            Status {
                phase: Phase::Applied,
                generation: Some(9)
            }
        );
        assert!(
            Operations::default()
                .commit(&after, &renewed, &bindings)
                .is_err()
        );
        let mut forged = after.clone();
        forged.0.get_mut(&request.id).unwrap().status.phase = Phase::Running;
        assert!(forged.commit(&after, &renewed, &bindings).is_err());
    }

    #[test]
    fn schema_seven_retains_operations_and_legacy_metadata_stays_legacy() {
        let mut logins = accounts::Logins::default();
        assert_ne!(
            aterm_json::from_str::<Value>(&logins.to_json()).unwrap()["schema"].as_u64(),
            Some(7)
        );
        logins.enrollment.admit(&request()).unwrap();
        let value = aterm_json::from_str::<Value>(&logins.to_json()).unwrap();
        assert_eq!(value["schema"].as_u64(), Some(7));
        let restored = accounts::Logins::from_json(&value.to_string(), 1794000000).unwrap();
        assert_eq!(restored.enrollment, logins.enrollment);
        let mut legacy = value.clone();
        legacy
            .as_object_mut()
            .unwrap()
            .insert("schema".into(), Value::from(6));
        assert!(accounts::Logins::from_json(&legacy.to_string(), 1794000000).is_err());
        let mut missing = value;
        missing.as_object_mut().unwrap().remove("enrollment");
        assert!(accounts::Logins::from_json(&missing.to_string(), 1794000000).is_err());
    }

    #[test]
    fn opaque_refusal_is_immutable_and_never_grants_a_provider_row() {
        let request = request();
        let intent = request.intent();
        let before = Operations::default();
        let mut book = before.clone();
        let refused = book.reject(&intent).unwrap();
        assert_eq!(
            refused,
            Status {
                phase: Phase::Refused,
                generation: None
            }
        );
        let mut without_proof = book.clone();
        without_proof.2.clear();
        assert!(
            without_proof
                .commit(&before, &Default::default(), &Default::default())
                .is_err()
        );
        book.commit(&before, &Default::default(), &Default::default())
            .unwrap();
        assert!(
            !book.admit(&request).unwrap(),
            "a source row appearing later cannot revive the ID"
        );
        assert!(book.require_running(&request).is_err());
        assert!(book.publishing(&request).is_err());
        assert!(book.admitted(&intent.id, &intent.caller).is_err());
        let staging = aterm_tempfile::tempdir().unwrap();
        let refused_logins = accounts::Logins {
            enrollment: book.clone(),
            ..Default::default()
        };
        assert!(
            accounts::write_enrollment_checkpoint(
                &request,
                "work@example.test",
                staging.path().to_str().unwrap(),
                accounts::Proof::Status,
                1794000000,
                &refused_logins
            )
            .is_err()
        );
        assert_eq!(
            staging.path().read_dir().unwrap().count(),
            0,
            "no provider checkpoint was created"
        );
        assert_eq!(book.reject(&intent).unwrap(), refused);
        let mut changed = intent.clone();
        changed.account_id = "f".repeat(64);
        assert!(book.reject(&changed).is_err());
        changed = intent.clone();
        changed.caller = "f".repeat(64);
        assert!(book.reject(&changed).is_err());
        changed = intent.clone();
        changed.revision = "f".repeat(64);
        assert!(book.reject(&changed).is_err());
        for phase in [Phase::Running, Phase::Applied] {
            let mut forged = book.clone();
            forged.0.get_mut(&intent.id).unwrap().status.phase = phase;
            assert!(Operations::decode(&forged.encode()).is_err());
            assert!(
                forged
                    .commit(&book, &Default::default(), &Default::default())
                    .is_err()
            );
        }
        let mut admitted = Operations::default();
        admitted.admit(&request).unwrap();
        let original = admitted.clone();
        assert_eq!(admitted.reject(&intent).unwrap().phase, Phase::Running);
        assert_eq!(admitted, original, "refusal never downgrades admitted work");
    }

    #[test]
    fn schema_eight_preserves_existing_admissions_and_binding_metadata() {
        let request = request();
        let mut logins = accounts::Logins::default();
        logins.labels.insert(
            "work".into(),
            Binding {
                dir: Some("/fixture/existing-protected-home".into()),
                renewal_generation: 7,
                last_success_generation: 7,
                ..Default::default()
            },
        );
        logins.generation_clock = 7;
        logins.enrollment.admit(&request).unwrap();
        let old = logins.to_json();
        assert_eq!(
            aterm_json::from_str::<Value>(&old).unwrap()["schema"].as_u64(),
            Some(7)
        );
        let mut restored = accounts::Logins::from_json(&old, 1794000000).unwrap();
        let before = restored.enrollment.clone();
        let mut rejected = request.intent();
        rejected.id = "d".repeat(32);
        rejected.account_id = "e".repeat(64);
        restored.enrollment.reject(&rejected).unwrap();
        restored
            .enrollment
            .commit(&before, &Default::default(), &Default::default())
            .unwrap();
        let encoded = restored.to_json();
        let mut value = aterm_json::from_str::<Value>(&encoded).unwrap();
        assert_eq!(value["schema"].as_u64(), Some(8));
        let roundtrip = accounts::Logins::from_json(&encoded, 1794000000).unwrap();
        assert_eq!(roundtrip.get("work"), logins.get("work"));
        assert_eq!(
            roundtrip
                .enrollment
                .lookup(&request)
                .unwrap()
                .unwrap()
                .phase,
            Phase::Running
        );
        assert_eq!(
            roundtrip
                .enrollment
                .retained(&rejected.id, &rejected.caller)
                .unwrap()
                .1
                .phase,
            Phase::Refused
        );
        value
            .as_object_mut()
            .unwrap()
            .insert("schema".into(), Value::from(7));
        assert!(
            accounts::Logins::from_json(&value.to_string(), 1794000000).is_err(),
            "new record kind cannot masquerade as old schema"
        );
    }

    #[test]
    fn bounded_custody_never_evicts_uncertain_or_completed_tombstones() {
        let mut book = Operations::default();
        for n in 0..MAX_OPERATIONS {
            let mut request = request();
            request.id = format!("{n:032x}");
            request.label = format!("work-{n}");
            book.admit(&request).unwrap();
        }
        let mut next = request();
        next.id = "f".repeat(32);
        assert!(book.admit(&next).is_err());
        assert!(
            book.reject(&next.intent()).is_err(),
            "capacity cannot invent a refusal proof"
        );
        assert_eq!(Operations::decode(&book.encode()).unwrap(), book);
    }
    #[test]
    fn cancelled_operation_cannot_recover_its_verified_completion_checkpoint() {
        let staging = aterm_tempfile::tempdir().unwrap();
        let request = request();
        let row = accounts::Row {
            label: "work".into(),
            email: Some("work@example.invalid".into()),
            ..Default::default()
        };
        let mut logins = accounts::Logins::default();
        logins.enrollment.admit(&request).unwrap();
        accounts::write_enrollment_checkpoint(
            &request,
            "work@example.invalid",
            staging.path().to_str().unwrap(),
            accounts::Proof::Status,
            1794000000,
            &logins,
        )
        .unwrap();
        let actual = staging.path().canonicalize().unwrap();
        let checkpoint = accounts::read_renewal_checkpoint_for(
            accounts::Cli::Codex,
            staging.path(),
            &actual,
            1794000001,
        )
        .unwrap();
        assert_eq!(checkpoint.enrollment.as_ref(), Some(&request));
        assert!(accounts::checkpoint_current(
            &row,
            &accounts::State::default(),
            &logins,
            &checkpoint,
            None
        ));
        logins
            .enrollment
            .finish(&request, Phase::CancelRequested)
            .unwrap();
        assert!(!accounts::checkpoint_current(
            &row,
            &accounts::State::default(),
            &logins,
            &checkpoint,
            None
        ));
        assert!(logins.enrollment.publishing(&request).is_err());
    }
}

#[cfg(all(test, unix))]
#[path = "codex_native_recovery_tests.rs"]
mod native_recovery_tests;
