// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Canonical native row-admission metadata. Deserialization, these hashes and
//! canonical binding absence confer no grant, source, or publication authority.
//! The owner authenticates the real issuer and the held writer joins occurrences.

use super::accounts;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_OTHERS: usize = 128;

fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(domain: &str, value: &impl Serialize) -> Result<String, String> {
    let bytes =
        aterm_json::to_vec(&(domain, value)).map_err(|_| "native row witness cannot be encoded")?;
    Ok(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(bytes),
    ))
}

/// Exact original pool-binding policy encoding, without unrelated row counters.
/// This hash is metadata; the transaction/issuer receipt supplies authority.
pub(super) fn binding_digest(logins: &accounts::Logins, label: &str) -> Result<String, String> {
    binding_digest_for(label, logins.get(label))
}

pub(super) fn binding_digest_for(
    label: &str,
    binding: Option<&accounts::Binding>,
) -> Result<String, String> {
    let value = binding.map(|binding| accounts::pool_authority_binding(label, binding));
    digest("aterm/native-row-binding/v1", &(label, value))
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BindingFence {
    label: String,
    generation: Option<u64>,
    success: Option<u64>,
    digest: String,
}
impl std::fmt::Debug for BindingFence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeBindingFence(metadata, redacted)")
    }
}
impl BindingFence {
    fn capture(logins: &accounts::Logins, label: &str) -> Result<Self, String> {
        let binding = logins.get(label);
        Ok(Self {
            label: label.into(),
            generation: binding.map(|b| b.renewal_generation),
            success: binding.map(|b| b.last_success_generation),
            digest: binding_digest(logins, label)?,
        })
    }
    fn valid(&self) -> bool {
        accounts::admissible_label(&self.label)
            && hex(&self.digest)
            && self.generation.is_some() == self.success.is_some()
            && self
                .success
                .zip(self.generation)
                .is_none_or(|(success, generation)| success <= generation)
    }
}

/// Immutable snapshot data saved only on a new canonical native Staging.
/// Exact workflow, selection, store/stage identities and predecessor are joined
/// from the containing Staging and the issuer-authenticated response.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeRowFence {
    schema: u32,
    roster_policy: String,
    own: BindingFence,
    other: Vec<BindingFence>,
    generation_clock: u64,
}
impl std::fmt::Debug for NativeRowFence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeRowFence(metadata, redacted)")
    }
}
impl NativeRowFence {
    pub(super) fn capture(
        raw: &accounts::Roster,
        logins: &accounts::Logins,
        label: &str,
    ) -> Result<Self, String> {
        if raw.row(label).is_none() {
            return Err("native row is not listed".into());
        }
        let labels: BTreeSet<_> = raw
            .rows
            .iter()
            .map(|row| row.label.as_str())
            .chain(logins.labels.keys().map(String::as_str))
            .filter(|other| *other != label)
            .collect();
        if labels.len() > MAX_OTHERS {
            return Err("native row witness capacity unavailable".into());
        }
        let fence = Self {
            schema: 1,
            roster_policy: digest(
                "aterm/native-row-roster/v1",
                &accounts::pool_authority_rows(raw)?,
            )?,
            own: BindingFence::capture(logins, label)?,
            other: labels
                .into_iter()
                .map(|other| BindingFence::capture(logins, other))
                .collect::<Result<_, _>>()?,
            generation_clock: logins.generation_clock,
        };
        if !fence.valid() {
            return Err("native row witness invalid".into());
        }
        Ok(fence)
    }
    pub(super) fn label(&self) -> &str {
        &self.own.label
    }
    pub(super) fn valid(&self) -> bool {
        self.schema == 1
            && hex(&self.roster_policy)
            && self.own.valid()
            && self.other.len() <= MAX_OTHERS
            && self.other.iter().enumerate().all(|(index, fence)| {
                fence.valid()
                    && fence.label != self.own.label
                    && self.other[..index]
                        .iter()
                        .all(|old| old.label < fence.label)
            })
            && self
                .own
                .generation
                .is_none_or(|generation| generation <= self.generation_clock)
            && self.other.iter().all(|fence| {
                fence
                    .generation
                    .is_none_or(|generation| generation <= self.generation_clock)
            })
    }
    /// Explain every intervening generation using an exact native issuer and
    /// the actual publication seal. No receipt, gap, duplicate, legacy delta or
    /// arbitrary edit can advance the simulated binding image.
    fn authenticate_changes(
        &self,
        env: &super::cli::Env,
        snapshot: &accounts::NativeRowSnapshot<'_>,
    ) -> Result<(), String> {
        snapshot.current()?;
        let records = snapshot.account_image();
        let configured_roster = env
            .accounts_roster_for(accounts::Cli::Codex)
            .ok_or("native row source unavailable")?;
        if std::fs::canonicalize(&configured_roster).map_err(|_| "native row source unavailable")?
            != snapshot.roster()
        {
            return Err("native row roster is not the admitted source".into());
        }
        let store_dir = std::fs::canonicalize(&snapshot.store().dir)
            .map_err(|_| "native row store unavailable")?;
        let configured = env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("native account store unavailable")?;
        if std::fs::canonicalize(&configured.dir).map_err(|_| "native account store unavailable")?
            != store_dir
        {
            return Err("native row publication source differs".into());
        }
        let store_identity = super::codex_enrollment::DirectoryIdentity::capture(&store_dir)?;
        self.changed_other(&records.raw_roster, &records.logins, &self.own.label)?;
        let mut expected: std::collections::BTreeMap<_, _> = self
            .other
            .iter()
            .map(|fence| (fence.label.clone(), fence.clone()))
            .collect();
        let mut clock = self.generation_clock;
        let publications = records.logins.enrollment.row_publications_after(clock)?;
        if publications.len() > 256 {
            return Err("native row history capacity unavailable".into());
        }
        for data in publications {
            if data.staging().store != store_dir
                || data.staging().store_identity.as_ref() != Some(&store_identity)
            {
                return Err("native row publication belongs to another store".into());
            }
            let binding = data.binding();
            if Some(binding.generation) != clock.checked_add(1)
                || binding.generation > records.logins.generation_clock
                || binding.request.label == self.own.label
            {
                return Err("native row clock contains an unexplained publication".into());
            }
            let before = data
                .staging()
                .native_row
                .as_ref()
                .ok_or("native publication has no row fence")?;
            let old = expected
                .get(&binding.request.label)
                .ok_or("native publication row is unknown")?;
            if before.roster_policy != self.roster_policy || before.own != *old {
                return Err("native publication predecessor changed".into());
            }
            let fresh =
                super::codex_grant_owner::recover(env, &binding.request, &binding.reference)?;
            let issuer =
                super::codex_grant_owner::row_publication_issuer(env, &binding.request, &fresh)?;
            verify_publication(&issuer, &data)?;
            expected.insert(
                binding.request.label.clone(),
                BindingFence {
                    label: binding.request.label.clone(),
                    generation: Some(binding.generation),
                    success: Some(binding.generation),
                    digest: data.binding_fingerprint().to_owned(),
                },
            );
            clock = binding.generation;
        }
        if clock != records.logins.generation_clock
            || expected.values().any(|fence| {
                BindingFence::capture(&records.logins, &fence.label)
                    .map_or(true, |current| current != *fence)
            })
        {
            return Err("native row changes are not authenticated publications".into());
        }
        snapshot.current()
    }

    /// Pure metadata comparison only. Every returned other-row difference still
    /// requires its authenticated derived-generation publication receipt under
    /// the same writer before a CurrentNativeRow proof can exist.
    pub(super) fn changed_other<'a>(
        &'a self,
        raw: &accounts::Roster,
        logins: &accounts::Logins,
        label: &str,
    ) -> Result<Vec<(&'a BindingFence, BindingFence)>, String> {
        if !self.valid()
            || self.own.label != label
            || self.roster_policy
                != digest(
                    "aterm/native-row-roster/v1",
                    &accounts::pool_authority_rows(raw)?,
                )?
            || self.own != BindingFence::capture(logins, label)?
            || logins.generation_clock < self.generation_clock
        {
            return Err("native row admission changed".into());
        }
        let current = Self::capture(raw, logins, label)?;
        if current.other.len() != self.other.len()
            || current
                .other
                .iter()
                .zip(&self.other)
                .any(|(new, old)| new.label != old.label)
        {
            return Err("native row binding membership changed".into());
        }
        Ok(self
            .other
            .iter()
            .zip(current.other)
            .filter(|(old, new)| **old != *new)
            .collect())
    }
}

/// Persisted data is not a proof: current use verifies both issuer and this
/// MAC-backed publication occurrence against the canonical held-writer state.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PublicationReceiptData {
    schema: u32,
    binding: super::codex_grant_owner::GrantBinding,
    staging: super::codex_enrollment::Staging,
    binding_digest: String,
    mac: String,
}
impl std::fmt::Debug for PublicationReceiptData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeRowPublicationData(redacted)")
    }
}
impl PublicationReceiptData {
    pub(super) fn valid(&self) -> bool {
        self.schema == 1
            && self.binding.generation > 0
            && self.binding.request.valid()
            && self.binding.reference.valid()
            && self.staging.request == self.binding.request
            && staging_shape(&self.staging)
            && self
                .staging
                .native_row
                .as_ref()
                .is_some_and(NativeRowFence::valid)
            && hex(&self.binding_digest)
            && hex(&self.mac)
    }
    pub(super) fn binding(&self) -> &super::codex_grant_owner::GrantBinding {
        &self.binding
    }
    pub(super) fn staging(&self) -> &super::codex_enrollment::Staging {
        &self.staging
    }
    pub(super) fn seal(&self) -> &str {
        &self.mac
    }
    pub(super) fn binding_fingerprint(&self) -> &str {
        &self.binding_digest
    }
}

/// Mint only under the actual generation-derived transaction and an opened,
/// authenticated schema-six code-response issuer. Serialized receipt data is
/// never accepted by this constructor.
pub(super) fn issue_publication(
    permit: &accounts::ActualPublicationPermit<'_>,
    request: &super::codex_enrollment::Request,
    fresh: &super::codex_grant_owner::RetainedGrant,
    issuer: &super::codex_grant_owner::RowPublicationIssuer,
) -> Result<RowPublicationReceipt, String> {
    if !fresh.matches_request(request) {
        return Err("native publication request differs".into());
    }
    let signature = issuer.sign_publication(permit, fresh)?;
    let (stamp, binding, staging, binding_digest, mac) = signature.into_parts();
    let data = PublicationReceiptData {
        schema: 1,
        binding,
        staging,
        binding_digest,
        mac,
    };
    if !data.valid() {
        return Err("native publication receipt is invalid".into());
    }
    Ok(RowPublicationReceipt { stamp, data })
}

/// Check both original producer authority and the exact derived publication
/// seal. This is a component of held-writer validation, not a standalone row
/// admission or permission to ignore current source policy.
pub(super) fn verify_publication(
    issuer: &super::codex_grant_owner::RowPublicationIssuer,
    data: &PublicationReceiptData,
) -> Result<(), String> {
    issuer.verify_publication(data)
}

/// Only native_row's authenticated issuer path may construct this proof. No
/// Clone/Deserialize/raw-data constructor; the permit channel consumes it once.
pub(super) struct RowPublicationReceipt {
    stamp: accounts::ActualPublicationStamp,
    data: PublicationReceiptData,
}
impl RowPublicationReceipt {
    pub(super) fn stamp(&self) -> &accounts::ActualPublicationStamp {
        &self.stamp
    }
    pub(super) fn into_record_proof(
        self,
        expected: &accounts::ActualPublicationStamp,
    ) -> Result<RecordPublicationReceipt, String> {
        if !self.stamp.same_transaction(expected) || !self.data.valid() {
            return Err("native row publication transaction changed".into());
        }
        Ok(RecordPublicationReceipt(self.data))
    }
}
/// No serde/data constructor. Only the actual one-shot permit channel can yield
/// this authenticated attachment for the same canonical operation Record.
pub(super) struct RecordPublicationReceipt(PublicationReceiptData);
impl RecordPublicationReceipt {
    pub(super) fn into_data(self) -> PublicationReceiptData {
        self.0
    }
}

/// Early admission occurrence, captured before the actual writer is released.
/// Canonical absence here is binding metadata, never source/owner absence.
pub(super) struct NativeRowOccurrence {
    request: super::codex_enrollment::Request,
    store: std::path::PathBuf,
    store_identity: super::codex_enrollment::DirectoryIdentity,
    fence: NativeRowFence,
    parent: accounts::RenewalParent,
    workflow: u64,
    policy: String,
    selection: Option<String>,
}
impl NativeRowOccurrence {
    pub(super) fn capture(
        snapshot: accounts::NativeRowSnapshot<'_>,
        request: &super::codex_enrollment::Request,
    ) -> Result<Self, String> {
        snapshot.current()?;
        let records = snapshot.account_image();
        records.logins.enrollment.require_running(request)?;
        if !records.logins.enrollment.requires_native_grant(request)? {
            return Err("native row operation is not admitted".into());
        }
        let workflow = records
            .raw_roster
            .row(&request.label)
            .and_then(|row| accounts::current_workflow(row, &records.logins))
            .ok_or("native row workflow unavailable")?
            .value();
        let occurrence = Self {
            request: request.clone(),
            store: std::fs::canonicalize(&snapshot.store().dir)
                .map_err(|_| "native row store unavailable")?,
            store_identity: super::codex_enrollment::DirectoryIdentity::capture(
                &snapshot.store().dir,
            )?,
            fence: NativeRowFence::capture(&records.raw_roster, &records.logins, &request.label)?,
            parent: accounts::RenewalParent::capture(&records.logins, &request.label),
            workflow,
            policy: accounts::pool_authority_revision(&records.raw_roster, &records.logins)?,
            selection: records
                .state
                .pending_pin
                .as_ref()
                .map(accounts::PendingSelection::identity),
        };
        snapshot.current()?;
        Ok(occurrence)
    }
    pub(super) fn validate_current(
        &self,
        env: &super::cli::Env,
        snapshot: &accounts::NativeRowSnapshot<'_>,
    ) -> Result<(), String> {
        let records = snapshot.account_image();
        if std::fs::canonicalize(&snapshot.store().dir).ok().as_ref() != Some(&self.store)
            || super::codex_enrollment::DirectoryIdentity::capture(&self.store)
                .ok()
                .as_ref()
                != Some(&self.store_identity)
            || !records
                .logins
                .enrollment
                .requires_native_grant(&self.request)?
            || records
                .raw_roster
                .row(&self.request.label)
                .and_then(|row| accounts::current_workflow(row, &records.logins))
                .is_none_or(|workflow| workflow.value() != self.workflow)
            || records
                .state
                .pending_pin
                .as_ref()
                .map(accounts::PendingSelection::identity)
                != self.selection
        {
            return Err("native row admission authority changed".into());
        }
        self.fence.authenticate_changes(env, snapshot)
    }

    pub(super) fn inspect_current<'a>(
        &self,
        env: &super::cli::Env,
        snapshot: accounts::NativeRowSnapshot<'a>,
    ) -> Result<LiveNativeRow<'a>, String> {
        self.validate_current(env, &snapshot)?;
        live_image(
            snapshot,
            self.request.clone(),
            self.workflow,
            self.selection.clone(),
            None,
        )
    }

    pub(super) fn fence(&self) -> &NativeRowFence {
        &self.fence
    }
    pub(super) fn policy(&self) -> &str {
        &self.policy
    }
    pub(super) fn finish(
        self,
        staging: super::codex_enrollment::Staging,
    ) -> Result<NativeRowAdmission, String> {
        if !staging_shape(&staging)
            || !staging.directories_current()
            || staging.request != self.request
            || staging.store != self.store
            || staging.store_identity.as_ref() != Some(&self.store_identity)
            || staging.native_row.as_ref() != Some(&self.fence)
            || staging.parent != self.parent
            || staging.workflow != self.workflow
            || staging.policy != self.policy
            || staging.selection != self.selection
        {
            return Err("native row occurrence changed before staging".into());
        }
        Ok(NativeRowAdmission { staging })
    }
}

/// Pre-response metadata admission from a live non-deserializable occurrence.
/// It has no signing or publication method and retains the actual writer.
pub(super) struct LiveNativeRow<'a> {
    snapshot: accounts::NativeRowSnapshot<'a>,
    request: super::codex_enrollment::Request,
    workflow: u64,
    selection: Option<String>,
    revision: String,
    staging: Option<super::codex_enrollment::Staging>,
}
fn live_image<'a>(
    snapshot: accounts::NativeRowSnapshot<'a>,
    request: super::codex_enrollment::Request,
    workflow: u64,
    selection: Option<String>,
    staging: Option<super::codex_enrollment::Staging>,
) -> Result<LiveNativeRow<'a>, String> {
    let revision = accounts::pool_authority_revision(
        &snapshot.account_image().raw_roster,
        &snapshot.account_image().logins,
    )?;
    let image = LiveNativeRow {
        snapshot,
        request,
        workflow,
        selection,
        revision,
        staging,
    };
    image.require_image(
        &image.snapshot.account_image().raw_roster,
        &image.snapshot.account_image().state,
        &image.snapshot.account_image().logins,
    )?;
    Ok(image)
}
impl LiveNativeRow<'_> {
    pub(super) fn policy(&self) -> &str {
        &self.revision
    }
    pub(super) fn require_image(
        &self,
        raw: &accounts::Roster,
        state: &accounts::State,
        logins: &accounts::Logins,
    ) -> Result<(), String> {
        self.snapshot.current()?;
        if accounts::pool_authority_revision(raw, logins)? != self.revision
            || !logins.enrollment.requires_native_grant(&self.request)?
            || logins.enrollment.staged(&self.request)? != self.staging.as_ref()
            || raw
                .row(&self.request.label)
                .and_then(|row| accounts::current_workflow(row, logins))
                .is_none_or(|workflow| workflow.value() != self.workflow)
            || state
                .pending_pin
                .as_ref()
                .map(accounts::PendingSelection::identity)
                != self.selection
        {
            return Err("native live row image changed".into());
        }
        Ok(())
    }
}

/// The canonical writer and authenticated original issuer remain owned through
/// the publication callback. This proves only the native row transition, not
/// source activation, provider independence or consumer readiness.
pub(super) struct CurrentNativeRow<'a> {
    snapshot: accounts::NativeRowSnapshot<'a>,
    issuer: &'a super::codex_grant_owner::RowPublicationIssuer,
    response: super::codex_grant_owner::PublicationResponse<'a>,
    revision: String,
}
impl CurrentNativeRow<'_> {
    pub(super) fn checkpoint_metadata(
        &self,
        logins: &accounts::Logins,
    ) -> Result<(&super::codex_enrollment::Staging, accounts::RenewalParent), String> {
        let image = self.snapshot.account_image();
        self.require_edit_image(&image.raw_roster, &image.state, logins)?;
        let admission = self.issuer.staging();
        Ok((admission, admission.parent))
    }

    #[cfg(unix)]
    pub(super) fn with_recovery_stage<R>(
        &self,
        use_stage: impl FnOnce(
            &[u8],
            &super::codex_app_login::StagedWitness,
            &super::codex_app_login::AuthLock,
            Option<Option<&str>>,
        ) -> Result<R, String>,
    ) -> Result<R, String> {
        self.snapshot.current()?;
        self.response.with_original_stage(use_stage)
    }

    pub(super) fn require_recovery(
        &self,
        admission: &NativeRecoveryAdmission,
    ) -> Result<(), String> {
        self.require_admission(admission.staging())?;
        if self.issuer.staging().request != *admission.request()
            || self.issuer.reference() != *admission.reference()
        {
            return Err("native recovery issuer changed".into());
        }
        self.snapshot
            .account_image()
            .logins
            .enrollment
            .require_native_publication_recovery(admission.request(), admission.ownership())
    }

    pub(super) fn initialize_owner(
        &self,
        env: &super::cli::Env,
        permit: &accounts::ActualPublicationPermit<'_>,
        fresh: &super::codex_grant_owner::RetainedGrant,
    ) -> Result<super::codex_grant_owner::OwnerWitnessTransition, String> {
        self.snapshot.current()?;
        self.response.initialize_revision(env, permit, fresh)
    }

    pub(super) fn require_admission(
        &self,
        staging: &super::codex_enrollment::Staging,
    ) -> Result<(), String> {
        self.snapshot.current()?;
        if staging != self.issuer.staging() || !staging.directories_current() {
            return Err("native row staging differs from original issuer".into());
        }
        Ok(())
    }
    pub(super) fn require_edit_image(
        &self,
        raw: &accounts::Roster,
        state: &accounts::State,
        logins: &accounts::Logins,
    ) -> Result<(), String> {
        self.snapshot.current()?;
        let staging = self.issuer.staging();
        if accounts::pool_authority_revision(raw, logins)? != self.revision
            || logins.enrollment.staged(&staging.request)? != Some(staging)
            || !raw
                .row(&staging.request.label)
                .and_then(|row| accounts::current_workflow(row, logins))
                .is_some_and(|workflow| workflow.value() == staging.workflow)
            || state
                .pending_pin
                .as_ref()
                .map(accounts::PendingSelection::identity)
                != staging.selection
        {
            return Err("native row canonical image changed".into());
        }
        logins.enrollment.require_running(&staging.request)
    }
    pub(super) fn attach_publication(
        &self,
        permit: &accounts::ActualPublicationPermit<'_>,
        fresh: &super::codex_grant_owner::RetainedGrant,
    ) -> Result<(), String> {
        self.snapshot.current()?;
        permit.attach_row_receipt(issue_publication(
            permit,
            &self.issuer.staging().request,
            fresh,
            self.issuer,
        )?)
    }
}

pub(super) fn with_current_row<R>(
    env: &super::cli::Env,
    (store, held): (&accounts::Store, &accounts::Lock),
    roster: &std::path::Path,
    now: i64,
    request: &super::codex_enrollment::Request,
    fresh: &super::codex_grant_owner::RetainedGrant,
    publish: impl FnOnce(CurrentNativeRow<'_>) -> Result<R, String>,
) -> Result<R, String> {
    let snapshot = store.native_row_snapshot_held(held, roster, now)?;
    let configured = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("native account store unavailable")?;
    let configured_dir =
        std::fs::canonicalize(&configured.dir).map_err(|_| "native account store unavailable")?;
    let store_dir =
        std::fs::canonicalize(&store.dir).map_err(|_| "native row store unavailable")?;
    if configured_dir != store_dir {
        return Err("native row store is not the admitted source".into());
    }
    let issuer = super::codex_grant_owner::row_publication_issuer(env, request, fresh)?;
    let staging = issuer.staging();
    let records = snapshot.account_image();
    if !staging.directories_current()
        || std::fs::canonicalize(&store.dir).ok().as_ref() != Some(&staging.store)
        || records.logins.enrollment.staged(request)? != Some(staging)
        || !records.logins.enrollment.requires_native_grant(request)?
        || records
            .raw_roster
            .row(&request.label)
            .and_then(|row| accounts::current_workflow(row, &records.logins))
            .is_none_or(|workflow| workflow.value() != staging.workflow)
        || records
            .state
            .pending_pin
            .as_ref()
            .map(accounts::PendingSelection::identity)
            != staging.selection
    {
        return Err("native row publication admission changed".into());
    }
    staging
        .native_row
        .as_ref()
        .ok_or("native row fence unavailable")?
        .authenticate_changes(env, &snapshot)?;
    let revision = accounts::pool_authority_revision(&records.raw_roster, &records.logins)?;
    let result = publish(CurrentNativeRow {
        snapshot,
        issuer: &issuer,
        response: issuer.publication_response()?,
        revision,
    })?;
    Ok(result)
}

/// Authenticated candidate for kernel-only publication recovery. It does not
/// claim a recovery generation or authorize credential/canonical writes.
pub(super) struct NativeRecoveryCandidate {
    source_home: std::path::PathBuf,
    request: super::codex_enrollment::Request,
    reference: super::codex_grant_owner::GrantReference,
    staging: super::codex_enrollment::Staging,
}
impl std::fmt::Debug for NativeRecoveryCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeRecoveryCandidate(redacted)")
    }
}
impl NativeRecoveryCandidate {
    pub(super) fn source_home(&self) -> &std::path::Path {
        &self.source_home
    }
    pub(super) fn request(&self) -> &super::codex_enrollment::Request {
        &self.request
    }
    pub(super) fn reference(&self) -> &super::codex_grant_owner::GrantReference {
        &self.reference
    }
    pub(super) fn staging(&self) -> &super::codex_enrollment::Staging {
        &self.staging
    }
    pub(super) fn check_store(&self, store: &accounts::Store) -> Result<(), String> {
        if store.cli() != accounts::Cli::Codex
            || std::fs::canonicalize(&store.dir).ok().as_ref() != Some(&self.staging.store)
            || !self.staging.directories_current()
        {
            return Err("native recovery store or stage changed".into());
        }
        Ok(())
    }
}

/// Exact canonical recovery occurrence. Only the held kernel resource path may
/// mint it after committing the matching NativePublication claim.
pub(super) struct NativeRecoveryAdmission {
    candidate: NativeRecoveryCandidate,
    claim: super::codex_enrollment::RecoveryOwnership,
}
impl std::fmt::Debug for NativeRecoveryAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeRecoveryAdmission(publication only, redacted)")
    }
}
impl NativeRecoveryAdmission {
    pub(super) fn request(&self) -> &super::codex_enrollment::Request {
        self.candidate.request()
    }
    pub(super) fn staging(&self) -> &super::codex_enrollment::Staging {
        self.candidate.staging()
    }
    pub(super) fn reference(&self) -> &super::codex_grant_owner::GrantReference {
        self.candidate.reference()
    }
    pub(super) fn claim(&self) -> u64 {
        self.claim.generation()
    }
    pub(super) fn ownership(&self) -> &super::codex_enrollment::RecoveryOwnership {
        &self.claim
    }
    pub(super) fn check_store(&self, store: &accounts::Store) -> Result<(), String> {
        if self.claim.generation() == 0 {
            return Err("native recovery has no claim".into());
        }
        self.candidate.check_store(store)
    }
}

pub(super) fn recovery_candidate(
    env: &super::cli::Env,
    (store, roster): (&accounts::Store, &std::path::Path),
    request: &super::codex_enrollment::Request,
    fresh: &super::codex_grant_owner::RetainedGrant,
) -> Result<NativeRecoveryCandidate, String> {
    let source_home = std::fs::canonicalize(
        env.home
            .as_deref()
            .ok_or("native recovery home unavailable")?,
    )
    .map_err(|_| "native recovery home unavailable")?;
    let held = store
        .acquire_within(&accounts::LOCK_LADDER)
        .map_err(|_| "native recovery writer unavailable")?
        .ok_or("native recovery writer busy")?;
    with_current_row(
        env,
        (store, &held),
        roster,
        env.now,
        request,
        fresh,
        |current| {
            current
                .snapshot
                .account_image()
                .logins
                .enrollment
                .require_native_recovery_candidate(request)?;
            Ok(NativeRecoveryCandidate {
                source_home,
                request: request.clone(),
                reference: fresh.reference(),
                staging: current.issuer.staging().clone(),
            })
        },
    )
}

pub(super) fn claim_retired_recovery(
    env: &super::cli::Env,
    (store, roster): (&accounts::Store, &std::path::Path),
    resources: super::native_enrollment_admission::NativeRecoveryResources,
    fresh: &super::codex_grant_owner::RetainedGrant,
    admitted_work: &super::codex_enrollment::Admission<'_>,
) -> Result<super::native_enrollment_admission::NativeRecoveryTurn, String> {
    resources.check_store(store)?;
    let candidate = resources.candidate();
    let held = store
        .acquire_within(&accounts::LOCK_LADDER)
        .map_err(|_| "native recovery writer unavailable")?
        .ok_or("native recovery writer busy")?;
    let claim = with_current_row(
        env,
        (store, &held),
        roster,
        env.now,
        candidate.request(),
        fresh,
        |current| {
            if current.issuer.staging() != candidate.staging()
                || current.issuer.reference() != *candidate.reference()
            {
                return Err("native recovery candidate changed".into());
            }
            let mut claim = None;
            store.transact_roster_logins_held(
                &held,
                roster,
                env.now,
                &mut None,
                |raw, state, logins| {
                    current.require_edit_image(raw, state, logins)?;
                    resources.check_store(store)?;
                    let next = logins
                        .enrollment
                        .claim_native_publication_recovery(&resources)?;
                    admitted_work.native_recovery(&next)?;
                    claim = Some(next);
                    Ok(Vec::new())
                },
            )?;
            claim.ok_or_else(|| String::from("native recovery claim unavailable"))
        },
    )?;
    let admission = NativeRecoveryAdmission {
        candidate: NativeRecoveryCandidate {
            source_home: candidate.source_home.clone(),
            request: candidate.request.clone(),
            reference: candidate.reference.clone(),
            staging: candidate.staging.clone(),
        },
        claim,
    };
    Ok(
        super::native_enrollment_admission::NativeRecoveryTurn::from_admission(
            resources, admission,
        ),
    )
}

/// Admission occurrence, never reconstructed by Deserialize. Its production
/// constructor belongs to the held canonical writer validation path.
pub(super) struct NativeRowAdmission {
    staging: super::codex_enrollment::Staging,
}
impl NativeRowAdmission {
    pub(super) fn inspect_current<'a>(
        &self,
        env: &super::cli::Env,
        snapshot: accounts::NativeRowSnapshot<'a>,
    ) -> Result<LiveNativeRow<'a>, String> {
        snapshot.current()?;
        let staging = &self.staging;
        if !staging.directories_current()
            || std::fs::canonicalize(&snapshot.store().dir).ok().as_ref() != Some(&staging.store)
        {
            return Err("native live staging changed".into());
        }
        staging
            .native_row
            .as_ref()
            .ok_or("native live row fence absent")?
            .authenticate_changes(env, &snapshot)?;
        live_image(
            snapshot,
            staging.request.clone(),
            staging.workflow,
            staging.selection.clone(),
            Some(staging.clone()),
        )
    }
    pub(super) fn staging(&self) -> &super::codex_enrollment::Staging {
        &self.staging
    }
}

pub(super) fn staging_shape(staging: &super::codex_enrollment::Staging) -> bool {
    staging.request.valid()
        && staging.store.is_absolute()
        && staging.directory.is_absolute()
        && staging.store_identity.is_some()
        && staging.directory_identity.is_some()
        && staging.workflow > 0
        && hex(&staging.policy)
        && staging
            .native_row
            .as_ref()
            .is_some_and(|fence| fence.valid() && fence.label() == staging.request.label)
}

// Admission requires DirectoryIdentity::capture and native descriptor custody,
// both of which deliberately refuse non-Unix platforms.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn occurrence_requires_real_writer_and_preserves_original_admission() {
        let world = aterm_tempfile::tempdir().unwrap();
        let home = world.path().join("home");
        let stage = world.path().join("stage");
        for path in [&home, &stage] {
            aterm_types::fs_restricted::ensure_private_dir(path).unwrap();
        }
        let store = accounts::Store::at_for(world.path(), accounts::Cli::Codex, &home);
        let roster = world.path().join("roster.toml");
        std::fs::write(
            &roster,
            "[[account]]\nlabel='work'\nemail='work@example.test'\nclass='dedicated'\n",
        )
        .unwrap();
        let request = super::super::codex_enrollment::Request {
            id: "a".repeat(32),
            caller: "b".repeat(64),
            label: "work".into(),
            revision: "c".repeat(64),
        };
        let now = 1_800_000_000;
        store
            .transact_logins(now, &mut None, |_, logins| {
                logins.enrollment.admit_native(&request)?;
                Ok(Vec::new())
            })
            .unwrap();
        let held = store.acquire_bounded().unwrap().unwrap();
        store
            .transact_pool_logins_held(&held, &roster, now, &mut None, |_, _, logins| {
                accounts::admit_workflow(logins, &request.label)?;
                Ok(Vec::new())
            })
            .unwrap();
        let foreign =
            accounts::Store::at_for(&world.path().join("other"), accounts::Cli::Codex, &home);
        assert!(
            foreign
                .native_row_snapshot_held(&held, &roster, now)
                .is_err()
        );
        let occurrence = NativeRowOccurrence::capture(
            store.native_row_snapshot_held(&held, &roster, now).unwrap(),
            &request,
        )
        .unwrap();
        let staging = super::super::codex_enrollment::Staging {
            native_row: Some(occurrence.fence().clone()),
            request: request.clone(),
            store: occurrence.store.clone(),
            directory: std::fs::canonicalize(&stage).unwrap(),
            store_identity: Some(occurrence.store_identity.clone()),
            directory_identity: Some(
                super::super::codex_enrollment::DirectoryIdentity::capture(&stage).unwrap(),
            ),
            parent: occurrence.parent,
            workflow: occurrence.workflow,
            policy: occurrence.policy().to_owned(),
            selection: occurrence.selection.clone(),
        };
        assert_eq!(
            occurrence.finish(staging.clone()).unwrap().staging(),
            &staging
        );
        let changed = NativeRowOccurrence::capture(
            store.native_row_snapshot_held(&held, &roster, now).unwrap(),
            &request,
        )
        .unwrap();
        let mut wrong = staging;
        wrong.policy = "d".repeat(64);
        assert!(changed.finish(wrong).is_err());
        let before = std::fs::read(store.logins_file()).unwrap();
        drop(held);
        assert_eq!(std::fs::read(store.logins_file()).unwrap(), before);
        assert!(!stage.join("auth.json").exists());
    }
}
