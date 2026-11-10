// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Account-admitted shared refresh and caller-scoped delivery. Provider work is
//! independent of delivery after its canonical attempt has been armed.

use super::*;
use crate::harness::codex_source_gate::SourceWorkLease;
use crate::harness::native_enrollment_admission::RefreshExchangeLease;

#[cfg(all(test, unix))]
std::thread_local! {
    static STOP_AFTER_PUBLISHING_ANCHOR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Own the established source-before-exchange order through response settlement.
/// Reconciliation borrows this guard instead of taking another shared kernel lock.
pub(super) struct RefreshWorkLease {
    // Retire principal/token resources, then E/L, then S. Only after every
    // resource is gone may the genuine operation's final count retire.
    lineage: Mutex<Option<Arc<RefreshLineageLease>>>,
    exchange: RefreshExchangeLease,
    source: Arc<SourceWorkLease>,
    source_identity: crate::harness::codex_source_gate::SourceGateIdentity,
    scope: Option<crate::harness::account_work::Scope>,
}
impl RefreshWorkLease {
    #[cfg(all(test, unix))]
    fn for_env(env: &Env) -> Result<Self, OwnerRefreshError> {
        Self::for_env_parts(env, None, None)
    }
    fn for_env_parts(
        env: &Env,
        scope: Option<crate::harness::account_work::Scope>,
        borrowed: Option<Arc<SourceWorkLease>>,
    ) -> Result<Self, OwnerRefreshError> {
        let home = env.home.as_deref().ok_or(OwnerRefreshError::Unavailable)?;
        let source = match borrowed {
            Some(source) => {
                if !source.matches_home(home) {
                    return Err(OwnerRefreshError::Unavailable);
                }
                source
            }
            None => Arc::new(
                SourceWorkLease::for_home(home).map_err(|_| OwnerRefreshError::Unavailable)?,
            ),
        };
        let source_identity = source
            .identity()
            .map_err(|_| OwnerRefreshError::Unavailable)?;
        let exchange =
            RefreshExchangeLease::for_env(env).map_err(|_| OwnerRefreshError::Unavailable)?;
        let held = Self {
            exchange,
            source,
            source_identity,
            lineage: Mutex::new(None),
            scope,
        };
        held.current().map_err(|_| OwnerRefreshError::Unavailable)?;
        Ok(held)
    }
    /// Metadata of the gate actually held at admission, never a cloned claim.
    pub(super) fn source_identity(&self) -> crate::harness::codex_source_gate::SourceGateIdentity {
        self.source_identity.clone()
    }
    pub(super) fn provider_current(&self) -> Result<(), String> {
        self.current()?;
        if self
            .scope
            .as_ref()
            .is_none_or(crate::harness::account_work::Scope::cancelled)
        {
            return Err("refresh operation is not admitted for a new provider request".into());
        }
        Ok(())
    }
    pub(super) fn owns_scope(&self, scope: &crate::harness::account_work::Scope) -> bool {
        self.scope
            .as_ref()
            .is_some_and(|actual| actual.operation_identity() == scope.operation_identity())
    }
    pub(super) fn current(&self) -> Result<(), String> {
        self.source.current().map_err(str::to_owned)?;
        self.exchange.current()?;
        let lineage = self
            .lineage
            .lock()
            .map_err(|_| "refresh lineage state unavailable")?
            .clone();
        if let Some(lineage) = lineage {
            lineage.current()?;
        }
        Ok(())
    }
    fn release_lineage(&self) -> Result<(), String> {
        self.lineage
            .lock()
            .map_err(|_| "refresh lineage state unavailable")?
            .take();
        Ok(())
    }
    fn reconcile_lineage(
        &self,
        env: &Env,
        binding: &GrantBinding,
    ) -> Result<Option<Arc<RefreshLineageLease>>, String> {
        self.release_lineage()?;
        if super::super::catalogue::existing_scope(env, &binding.request.id)?.is_none() {
            return Ok(None);
        }
        let held = Arc::new(RefreshLineageLease::for_reconciliation(env, binding, self)?);
        *self
            .lineage
            .lock()
            .map_err(|_| "refresh lineage state unavailable")? = Some(held.clone());
        Ok(Some(held))
    }
    pub(super) fn send_lineage(
        &self,
        env: &Env,
        binding: &GrantBinding,
    ) -> Result<Arc<RefreshLineageLease>, String> {
        self.provider_current()?;
        self.release_lineage()?;
        let held = Arc::new(RefreshLineageLease::for_send(env, binding, self)?);
        *self
            .lineage
            .lock()
            .map_err(|_| "refresh lineage state unavailable")? = Some(held.clone());
        Ok(held)
    }
}
use super::lineage::RefreshLineageLease;
use std::io::Read as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn delivery_operation(
    binding: &GrantBinding,
    caller: &slot::Caller,
    request: &slot::RefreshRequest,
) -> Result<Operation, OwnerRefreshError> {
    let authority = request.authority().ok_or(OwnerRefreshError::Unavailable)?;
    if authority.binding() != binding {
        return Err(OwnerRefreshError::Unavailable);
    }
    let operation = Operation {
        id: request.operation.clone(),
        caller: caller.clone(),
        slot: request.slot.clone(),
        expected_digest: request.expected_digest.clone(),
        assignment_generation: authority.assignment_generation(),
        source_authority: authority.source_authority().clone(),
        credential_generation: authority.materialized_revision().generation,
    };
    if !operation.valid() {
        return Err(OwnerRefreshError::Unavailable);
    }
    Ok(operation)
}

fn same_wire_operation(a: &Operation, b: &Operation) -> bool {
    a.id == b.id
        && a.caller == b.caller
        && a.slot == b.slot
        && a.expected_digest == b.expected_digest
        && a.assignment_generation == b.assignment_generation
        && a.source_authority == b.source_authority
}

fn current_source(
    env: &Env,
    binding: &GrantBinding,
    operation: &Operation,
    namespace: Option<&slot::RefreshNamespace>,
) -> Result<(), String> {
    if let Some(namespace) = namespace {
        return namespace.with_current(
            env,
            &operation.caller,
            binding,
            operation.assignment_generation,
            &operation.source_authority,
            || Ok(()),
        );
    }
    crate::harness::codex_assignment::with_current_assignment(
        env,
        &operation.caller,
        binding,
        operation.assignment_generation,
        &operation.source_authority,
        || Ok(()),
    )
}

fn completed_operation<'a>(
    journal: &'a Journal,
    operation: &Operation,
) -> Result<Option<&'a Completed>, OwnerRefreshError> {
    match journal
        .completed
        .iter()
        .find(|done| done.operation.id() == operation.id)
    {
        Some(done)
            if done
                .operation
                .session()
                .is_some_and(|recorded| same_wire_operation(recorded, operation)) =>
        {
            Ok(Some(done))
        }
        Some(_) => Err(OwnerRefreshError::Unavailable),
        None => Ok(None),
    }
}

fn known_digest(journal: &Journal, digest: &str) -> bool {
    journal.current.digest == digest
        || journal
            .pending
            .as_ref()
            .is_some_and(|pending| pending.base_digest == digest)
        || journal
            .completed
            .iter()
            .any(|done| done.digest == digest || done.operation.expected_digest() == digest)
}

/// Reserve both the old prepared owner's durable receipt and this distinct
/// caller's coalesced receipt before allowing any irreversible provider action.
fn require_receipt_capacity(
    journal: &Journal,
    operation: &Operation,
) -> Result<(), OwnerRefreshError> {
    if completed_operation(journal, operation)?.is_some() {
        return Ok(());
    }
    let receipts = if journal
        .pending
        .as_ref()
        .is_some_and(|pending| pending.operation.id() != operation.id)
    {
        2
    } else {
        1
    };
    if journal
        .completed
        .len()
        .checked_add(receipts)
        .is_none_or(|count| count > MAX_OPERATIONS)
    {
        return Err(OwnerRefreshError::Unavailable);
    }
    Ok(())
}

/// A committed owner receipt is not Slot readiness. Keep the real materializer
/// claims alive through the parent's final peer/source recheck before returning R.
fn materialize_verified<'a, R>(
    grant: AdmittedGrant<'a>,
    operation: &Operation,
    materialize: impl FnOnce(AdmittedGrant<'a>) -> Result<(R, slot::MaterializationReceipt), String>,
) -> Result<(R, slot::MaterializationReceipt, slot::Revision), String> {
    let revision = grant.revision().clone();
    let (result, receipt) = materialize(grant)?;
    if receipt.caller() != &operation.caller
        || receipt.slot() != operation.slot
        || receipt.generation() != revision.generation
        || receipt.digest() != revision.digest
    {
        return Err("native refresh materialization differs".into());
    }
    Ok((result, receipt, revision))
}

pub(super) fn persist_owned(
    claimed: Claimed,
    retained: &mut Option<Box<OwnedRefreshRetention>>,
) -> Result<Claimed, String> {
    match claimed.persist() {
        Ok(claimed) => Ok(claimed),
        Err(failure) => {
            *retained = Some(failure);
            Err("native response retention is pending".into())
        }
    }
}

fn cipher_digest(claimed: &Claimed) -> Result<String, String> {
    Ok(aterm_codec::hex::encode_lower(
        &claimed
            .store
            .sealed_digest()
            .ok_or("owner journal absent")?,
    ))
}

/// The caller retains the parent's actual AuthLock and source admission. Only
/// already authenticated current/previous candidates may replace this cache.
fn synchronize_canonical(claimed: &Claimed, canonical_auth: &Path) -> Result<(), String> {
    let journal = claimed.journal.as_ref().ok_or("owner journal absent")?;
    let current = journal.current.open()?;
    let previous = journal
        .previous_current
        .as_ref()
        .map(Current::open)
        .transpose()?;
    let parent = canonical_auth
        .parent()
        .ok_or("canonical auth unavailable")?;
    if canonical_auth
        .file_name()
        .is_none_or(|name| name != "auth.json")
    {
        return Err("canonical auth unavailable".into());
    }
    let actual = SecretBytes::new(
        platform::read_private(parent, "auth.json", MAX_DOCUMENT)
            .map_err(|_| "canonical auth unavailable")?,
    );
    let matches_current = aterm_digest::ct_eq(actual.as_slice(), current.document.as_slice());
    if !matches_current
        && !previous
            .as_ref()
            .is_some_and(|old| aterm_digest::ct_eq(actual.as_slice(), old.document.as_slice()))
    {
        return Err("canonical auth differs from authenticated owner".into());
    }
    if !matches_current {
        platform::write_private(parent, "auth.json", current.document.as_slice())
            .map_err(|_| "canonical owner cache publication unavailable")?;
    }
    let (file, metadata) = platform::open_private(parent, "auth.json")
        .map_err(|_| "canonical owner cache unavailable")?;
    if metadata.len() > MAX_DOCUMENT as u64 {
        return Err("canonical owner cache unavailable".into());
    }
    let mut raw = Vec::with_capacity(metadata.len() as usize);
    (&file)
        .take(MAX_DOCUMENT as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "canonical owner cache unavailable")?;
    let bytes = SecretBytes::new(raw);
    if !aterm_digest::ct_eq(bytes.as_slice(), current.document.as_slice()) {
        return Err("canonical owner cache differs".into());
    }
    aterm_durable::Flush::Durable
        .file(&file)
        .map_err(|_| "canonical owner cache durability unknown")?;
    aterm_durable::Flush::Durable
        .dir(parent)
        .map_err(|_| "canonical owner cache durability unknown")?;
    if !astream_sys::file_matches_path(&file, canonical_auth).unwrap_or(false) {
        return Err("canonical owner cache changed".into());
    }
    Ok(())
}

fn reconcile_held_with_lineage(
    env: &Env,
    binding: &GrantBinding,
    anchor: &mut codex_enrollment::OwnerAnchorWriter<'_>,
    canonical_auth: &Path,
    retained: &mut Option<Box<OwnedRefreshRetention>>,
    lineage: Option<&RefreshLineageLease>,
) -> Result<bool, String> {
    let mut claimed = Claimed::open(env, binding, false)?.ok_or("owner journal absent")?;
    let mut changed = false;
    for _ in 0..8 {
        let witness = anchor.read()?.ok_or("canonical owner anchor absent")?;
        claimed.view(&witness)?;
        if super::super::catalogue::existing_scope(env, &binding.request.id)?.is_some() {
            let current = claimed
                .journal
                .as_ref()
                .ok_or("owner journal absent")?
                .current
                .open()?;
            let admitted = lineage
                .ok_or_else(|| "global reconciliation lacks actual lineage resources".to_owned())
                .and_then(|lineage| {
                    lineage.assess_catalogue(
                        env,
                        binding,
                        &current.identity,
                        current.document.as_slice(),
                    )
                });
            if matches!(
                &admitted,
                Ok(super::super::catalogue::RefreshSourceStatus::Conflicted)
            ) {
                let phase = claimed
                    .journal
                    .as_ref()
                    .and_then(|journal| journal.pending.as_ref())
                    .map(|pending| pending.phase);
                if phase != Some(Phase::Responded) {
                    // The authenticated alias evidence remains durable. No cache delivery,
                    // new POST, success, or plaintext/count retention is needed
                    // to defer a known alias behind its existing spent fence.
                    return Err("refresh alias remains durably fenced".into());
                }
                // Responded reaches the candidate branch below solely to
                // persist KnownAlias quarantine, never to publish its bytes.
            }
            if admitted.is_err() {
                *retained = Some(Box::new(OwnedRefreshRetention {
                    claimed,
                    plaintext: None,
                    raw: None,
                    no_send: None,
                    work_lease: None,
                }));
                return Err("refresh lineage reconciliation pending".into());
            }
        }
        let exact = cipher_digest(&claimed)? == witness.journal_digest;
        if !exact {
            let journal = claimed.journal.as_mut().ok_or("owner journal absent")?;
            // A pre-anchor Attempted journal could not legally send. Restore
            // Prepared rather than manufacturing uncertainty by adopting it.
            if journal.transition.kind == TransitionKind::Attempt
                && witness.pending.is_none()
                && journal
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.phase == Phase::Attempted)
            {
                journal.pending.as_mut().expect("checked pending").phase = Phase::Prepared;
                journal.transition = JournalTransition {
                    expected: Some(witness.clone()),
                    kind: TransitionKind::Attempt,
                };
                claimed = persist_owned(claimed, retained)?;
            }
            if claimed
                .journal
                .as_ref()
                .is_some_and(|journal| journal.transition.kind == TransitionKind::Promote)
            {
                synchronize_canonical(&claimed, canonical_auth)?;
            }
            let transition = claimed.transition()?;
            anchor.compare_exchange(&transition)?;
            #[cfg(all(test, unix))]
            if STOP_AFTER_PUBLISHING_ANCHOR.get()
                && claimed.journal.as_ref().is_some_and(|journal| {
                    journal
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.phase == Phase::Publishing)
                })
            {
                // Actual encrypted response and canonical C2 CAS are durable.
                // Model death before the next physical/journal cleanup step.
                return Err("test cut after durable publishing anchor".into());
            }
            changed = true;
            continue;
        }
        let phase = claimed
            .journal
            .as_ref()
            .and_then(|journal| journal.pending.as_ref().map(|pending| pending.phase));
        match phase {
            Some(Phase::Responded) => {
                let journal = claimed.journal.as_mut().ok_or("owner journal absent")?;
                let pending = journal.pending.as_ref().ok_or("owner response absent")?;
                let response = pending.response.as_ref().ok_or("owner response absent")?;
                let old = journal.current.open()?;
                let raw = response.bytes()?;
                let merged = response
                    .received_at
                    .ok_or_else(|| "refresh receipt time unavailable".to_owned())
                    .and_then(|received| {
                        codex_app_login::merge_owned_refresh(
                            old.document.as_slice(),
                            response.status,
                            raw.as_slice(),
                            received,
                        )
                    });
                match merged {
                    Ok(document) => {
                        let admitted = if let Some(lineage) = lineage {
                            lineage.assess_catalogue(
                                env,
                                binding,
                                &old.identity,
                                document.as_slice(),
                            )
                        } else if super::super::catalogue::existing_scope(env, &binding.request.id)?
                            .is_some()
                        {
                            Err("global refresh promotion lacks lineage resources".into())
                        } else {
                            Ok(super::super::catalogue::RefreshSourceStatus::NoKnownAlias)
                        };
                        if matches!(
                            &admitted,
                            Ok(super::super::catalogue::RefreshSourceStatus::Conflicted)
                        ) {
                            journal.schema = 4;
                            let pending = journal.pending.as_mut().expect("checked response");
                            pending.phase = Phase::Quarantined;
                            pending.quarantine_reason = Some(QuarantineReason::KnownAlias);
                            journal.transition = JournalTransition {
                                expected: Some(witness),
                                kind: TransitionKind::Response,
                            };
                            claimed = persist_owned(claimed, retained)?;
                            anchor.compare_exchange(&claimed.transition()?)?;
                            // The exact completed response and spent-token
                            // fence are now durable. No memory/source count is
                            // needed to pretend this denial is a success.
                            return Err("refresh alias is durably quarantined".into());
                        }
                        if admitted.is_err() {
                            *retained = Some(Box::new(OwnedRefreshRetention {
                                claimed,
                                plaintext: None,
                                raw: None,
                                no_send: None,
                                work_lease: None,
                            }));
                            return Err("refresh lineage reconciliation pending".into());
                        }
                        let generation = journal
                            .current
                            .generation
                            .checked_add(1)
                            .ok_or("owner revision exhausted")?;
                        let document_text = aterm_codec::base64::encode(document.as_slice())
                            .map_err(|_| "owner response unavailable")?;
                        let candidate = Current {
                            identity: journal.current.identity.clone(),
                            generation,
                            digest: aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
                                document.as_slice(),
                            )),
                            document: document_text,
                        };
                        candidate.open()?;
                        journal.previous_current =
                            Some(std::mem::replace(&mut journal.current, candidate));
                        journal.pending.as_mut().expect("checked pending").phase =
                            Phase::Publishing;
                        journal.transition = JournalTransition {
                            expected: Some(witness),
                            kind: TransitionKind::Promote,
                        };
                    }
                    Err(_) => {
                        journal.pending.as_mut().expect("checked pending").phase =
                            Phase::Quarantined;
                        journal.transition = JournalTransition {
                            expected: Some(witness),
                            kind: TransitionKind::Response,
                        };
                    }
                }
                claimed = persist_owned(claimed, retained)?;
                changed = true;
            }
            Some(Phase::Publishing) => {
                synchronize_canonical(&claimed, canonical_auth)?;
                let journal = claimed.journal.as_mut().ok_or("owner journal absent")?;
                if journal.completed.len() >= MAX_OPERATIONS {
                    return Err("owner operation capacity exhausted".into());
                }
                let pending = journal.pending.take().ok_or("owner publication absent")?;
                if journal
                    .completed
                    .iter()
                    .any(|done| done.operation.id() == pending.operation.id())
                {
                    return Err("owner operation already exists".into());
                }
                journal.completed.push(Completed {
                    operation: pending.operation,
                    generation: journal.current.generation,
                    digest: journal.current.digest.clone(),
                    spent_token_digest: Some(
                        pending
                            .token_digest
                            .ok_or("owner spent token evidence absent")?,
                    ),
                });
                journal.previous_current = None;
                journal.transition = JournalTransition {
                    expected: Some(witness),
                    kind: TransitionKind::Metadata,
                };
                claimed = persist_owned(claimed, retained)?;
                changed = true;
            }
            _ => return Ok(changed),
        }
    }
    Err("owner reconciliation did not settle".into())
}

#[cfg(all(test, unix))]
fn with_refresh_work<R>(
    env: &Env,
    run: impl FnOnce(&RefreshWorkLease) -> Result<R, OwnerRefreshError>,
) -> Result<R, OwnerRefreshError> {
    with_refresh_work_parts(env, None, None, run)
}
pub(super) fn with_refresh_work_scoped<R>(
    env: &Env,
    scope: &crate::harness::account_work::Scope,
    borrowed: Option<Arc<SourceWorkLease>>,
    run: impl FnOnce(&RefreshWorkLease) -> Result<R, OwnerRefreshError>,
) -> Result<R, OwnerRefreshError> {
    with_refresh_work_parts(env, Some(scope.clone()), borrowed, run)
}
fn with_refresh_work_parts<R>(
    env: &Env,
    scope: Option<crate::harness::account_work::Scope>,
    borrowed: Option<Arc<SourceWorkLease>>,
    run: impl FnOnce(&RefreshWorkLease) -> Result<R, OwnerRefreshError>,
) -> Result<R, OwnerRefreshError> {
    let lease = RefreshWorkLease::for_env_parts(env, scope, borrowed)?;
    lease
        .current()
        .map_err(|_| OwnerRefreshError::Unavailable)?;
    match run(&lease) {
        Err(OwnerRefreshError::Retaining(retention)) => {
            Err(OwnerRefreshError::Retaining(retention.retain_work(lease)))
        }
        outcome => {
            lease
                .current()
                .map_err(|_| OwnerRefreshError::Unavailable)?;
            outcome
        }
    }
}

fn reconcile_current_held_work(
    env: &Env,
    binding: &GrantBinding,
    caller: &slot::Caller,
    source: &RefreshWorkLease,
) -> Result<bool, OwnerRefreshError> {
    source
        .current()
        .map_err(|_| OwnerRefreshError::Unavailable)?;
    if let Some(scope) = source.scope.as_ref() {
        return reconcile_retained_held_work(env, binding, scope, source).map(|()| true);
    }
    let lineage = source
        .reconcile_lineage(env, binding)
        .map_err(|_| OwnerRefreshError::Unavailable)?;
    let mut retained = None;
    let result = super::super::with_current_published_grant(env, binding, caller, |mut grant| {
        let canonical_auth = grant.canonical_auth.to_owned();
        reconcile_held_with_lineage(
            env,
            binding,
            grant.anchor(),
            &canonical_auth,
            &mut retained,
            lineage.as_deref(),
        )
    });
    if let Some(retained) = retained {
        return Err(OwnerRefreshError::Retaining(retained));
    }
    source
        .current()
        .map_err(|_| OwnerRefreshError::Unavailable)?;
    result.map_err(|_| OwnerRefreshError::Unavailable)
}

/// Finish only an already-owned response after the sender/Slot has retired.
/// The existing app worker scope remains retained even when it is cancelled;
/// this path grants no provider action or consumer materialization authority.
pub(super) fn reconcile_retained(
    env: &Env,
    binding: &GrantBinding,
    scope: &crate::harness::account_work::Scope,
) -> Result<(), OwnerRefreshError> {
    with_refresh_work_scoped(env, scope, None, |source| {
        reconcile_retained_held_work(env, binding, scope, source)
    })
}

/// A deferred locator must reacquire the exact original physical namespace.
/// Gate equality is checked under the newly held claim before decrypting owner
/// credentials; metadata alone confers no provider or delivery permission.
pub(super) fn reconcile_retained_in_source(
    env: &Env,
    binding: &GrantBinding,
    scope: &crate::harness::account_work::Scope,
    expected: &crate::harness::codex_source_gate::SourceGateIdentity,
) -> Result<(), OwnerRefreshError> {
    let home = env.home.as_deref().ok_or(OwnerRefreshError::Unavailable)?;
    let held =
        SourceWorkLease::for_existing_home(home).map_err(|_| OwnerRefreshError::Unavailable)?;
    if held
        .identity()
        .map_err(|_| OwnerRefreshError::Unavailable)?
        != *expected
    {
        return Err(OwnerRefreshError::Unavailable);
    }
    with_refresh_work_scoped(env, scope, Some(Arc::new(held)), |source| {
        if source.source_identity() != *expected {
            return Err(OwnerRefreshError::Unavailable);
        }
        reconcile_retained_held_work(env, binding, scope, source)
    })
}

pub(super) fn reconcile_retained_held_work(
    env: &Env,
    binding: &GrantBinding,
    scope: &crate::harness::account_work::Scope,
    source: &RefreshWorkLease,
) -> Result<(), OwnerRefreshError> {
    let scope = source.scope.as_ref().unwrap_or(scope);
    source
        .current()
        .map_err(|_| OwnerRefreshError::Unavailable)?;
    #[cfg(all(test, unix))]
    super::native_usage::test_stage("maintenance_lineage_inventory");
    let lineage = source
        .reconcile_lineage(env, binding)
        .map_err(|_| OwnerRefreshError::Unavailable)?;
    let mut retained = None;
    #[cfg(all(test, unix))]
    super::native_usage::test_stage("maintenance_canonical_owner");
    let result = super::super::with_maintenance_owner(env, binding, scope, |mut grant| {
        #[cfg(all(test, unix))]
        super::native_usage::test_stage("maintenance_journal_reconcile");
        let canonical_auth = grant.canonical_auth().to_owned();
        reconcile_held_with_lineage(
            env,
            binding,
            grant.anchor(),
            &canonical_auth,
            &mut retained,
            lineage.as_deref(),
        )
        .map(|_| ())
    });
    if let Some(retained) = retained {
        return Err(OwnerRefreshError::Retaining(retained));
    }
    source
        .current()
        .map_err(|_| OwnerRefreshError::Unavailable)?;
    result.map_err(|_| OwnerRefreshError::Unavailable)
}

pub(super) struct PreparedExchange {
    pub(super) claimed: Claimed,
    pub(super) witness: OwnerWitness,
    pub(super) document: SecretBytes,
}

enum Preparation {
    Ready,
    Exchange(Box<PreparedExchange>),
    Reconcile,
}

fn prepare(
    env: &Env,
    binding: &GrantBinding,
    caller: &slot::Caller,
    operation: &Operation,
    lineage: &RefreshLineageLease,
    namespace: Option<&slot::RefreshNamespace>,
) -> Result<Preparation, OwnerRefreshError> {
    let mut retained = None;
    let mut preparation = None;
    let admitted = super::super::with_current_published_grant(env, binding, caller, |mut grant| {
        let witness = grant
            .anchor()
            .read()?
            .ok_or("canonical owner anchor absent")?;
        let mut claimed = Claimed::open(env, binding, false)?.ok_or("owner journal absent")?;
        let current = claimed.view(&witness)?;
        lineage.matches(
            binding,
            &current.identity,
            &current.revision,
            current.document.as_slice(),
        )?;
        current_source(env, binding, operation, namespace)?;
        lineage.check_catalogue(env, binding, &current.identity, current.document.as_slice())?;
        if current.revision.generation < operation.credential_generation {
            return Err("slot revision exceeds owner revision".into());
        }
        let journal = claimed.journal.as_ref().ok_or("owner journal absent")?;
        if completed_operation(journal, operation)
            .map_err(|_| "refresh operation changed")?
            .is_some()
        {
            preparation = Some(Preparation::Ready);
            return Ok(());
        }
        if journal.pending.as_ref().is_some_and(|pending| {
            pending.operation.id() == operation.id
                && !pending
                    .operation
                    .session()
                    .is_some_and(|recorded| same_wire_operation(recorded, operation))
        }) {
            return Err("refresh operation changed".into());
        }
        if !known_digest(journal, &operation.expected_digest) {
            return Err("refresh source digest is unknown".into());
        }
        if journal
            .pending
            .as_ref()
            .is_some_and(|pending| matches!(pending.phase, Phase::Responded | Phase::Publishing))
        {
            preparation = Some(Preparation::Reconcile);
            return Ok(());
        }
        require_receipt_capacity(journal, operation)
            .map_err(|_| "owner operation capacity exhausted")?;
        if current.revision.digest != operation.expected_digest
            || current.revision.generation > operation.credential_generation
        {
            let journal = claimed.journal.as_mut().expect("checked journal");
            journal.completed.push(Completed {
                operation: operation.clone().into(),
                generation: current.revision.generation,
                digest: current.revision.digest.clone(),
                spent_token_digest: None,
            });
            let kind = match journal.pending.as_ref().map(|pending| pending.phase) {
                None => TransitionKind::Metadata,
                Some(Phase::Prepared | Phase::Attempted) => TransitionKind::Attempt,
                Some(Phase::Rejected | Phase::Quarantined) => TransitionKind::Response,
                _ => return Err("owner response requires reconciliation".into()),
            };
            journal.transition = JournalTransition {
                expected: Some(witness),
                kind,
            };
            claimed = persist_owned(claimed, &mut retained)?;
            let transition = claimed.transition()?;
            grant.anchor().compare_exchange(&transition)?;
            preparation = Some(Preparation::Ready);
            return Ok(());
        }
        let token_digest =
            codex_app_login::owned_refresh_token_digest(current.document.as_slice())?;
        let journal = claimed.journal.as_mut().expect("checked journal");
        match journal.pending.as_ref() {
            Some(pending)
                if pending.phase == Phase::Prepared
                    && pending.base_generation == current.revision.generation
                    && pending.base_digest == current.revision.digest
                    && pending.operation.expected_digest() == current.revision.digest
                    && pending.token_digest.as_deref() == Some(token_digest.as_str()) => {}
            Some(_) => return Err("owner refresh is uncertain or unavailable".into()),
            None => {
                journal.pending = Some(Pending {
                    operation: operation.clone().into(),
                    phase: Phase::Prepared,
                    quarantine_reason: None,
                    base_generation: current.revision.generation,
                    base_digest: current.revision.digest.clone(),
                    token_digest: Some(token_digest),
                    response: None,
                })
            }
        }
        // A reserved Prepared operation is not a send. Parent postcheck failure
        // leaves it safe for a later currently admitted caller of this grant.
        journal.transition = JournalTransition {
            expected: Some(witness.clone()),
            kind: TransitionKind::Attempt,
        };
        claimed = persist_owned(claimed, &mut retained)?;
        let transition = claimed.transition()?;
        let next = transition.next().clone();
        if next.pending.is_none() {
            return Err("owner prepared reservation is absent".into());
        }
        grant.anchor().compare_exchange(&transition)?;
        preparation = Some(Preparation::Exchange(Box::new(PreparedExchange {
            claimed,
            witness: next,
            document: current.document,
        })));
        Ok(())
    });
    if let Some(retained) = retained {
        return Err(OwnerRefreshError::Retaining(retained));
    }
    admitted.map_err(|_| OwnerRefreshError::Unavailable)?;
    preparation.ok_or(OwnerRefreshError::Unavailable)
}

fn deliver<R>(
    env: &Env,
    binding: &GrantBinding,
    caller: &slot::Caller,
    operation: &Operation,
    namespace: Option<&slot::RefreshNamespace>,
    materialize: impl FnOnce(AdmittedGrant<'_>) -> Result<(R, slot::MaterializationReceipt), String>,
) -> Result<(R, slot::Revision), OwnerRefreshError> {
    let mut delivered = None;
    let admitted = super::super::with_current_published_grant(env, binding, caller, |mut grant| {
        let witness = grant
            .anchor()
            .read()?
            .ok_or("canonical owner anchor absent")?;
        let claimed = Claimed::open(env, binding, false)?.ok_or("owner journal absent")?;
        let current = claimed.view(&witness)?;
        let journal = claimed.journal.as_ref().ok_or("owner journal absent")?;
        let completed = completed_operation(journal, operation)
            .map_err(|_| "refresh operation changed")?
            .ok_or("refresh receipt absent")?;
        if current.revision != *grant.revision()
            || current.revision.generation < completed.generation
            || current.revision.generation < operation.credential_generation
        {
            return Err("refresh owner revision changed".into());
        }
        let recorded = completed
            .operation
            .session()
            .ok_or("native quota receipt is not session delivery")?
            .clone();
        drop(claimed);
        current_source(env, binding, operation, namespace)?;
        delivered = Some(materialize_verified(grant, &recorded, materialize)?);
        current_source(env, binding, operation, namespace)?;
        Ok(())
    });
    admitted.map_err(|_| OwnerRefreshError::Unavailable)?;
    let (result, receipt, revision) = delivered.ok_or(OwnerRefreshError::Unavailable)?;
    // The actual slot claims survived the parent's final peer recheck. The
    // immutable owner receipt alone never supplied this readiness proof.
    drop(receipt);
    Ok((result, revision))
}

/// Hang detector for the admitted owner's refresh; native authority is checked separately.
const REFRESH_BUDGET: Duration = Duration::from_secs(60);

pub(in crate::harness) fn with_refreshed_owner<R>(
    env: &Env,
    binding: &GrantBinding,
    caller: &slot::Caller,
    request: &slot::RefreshRequest,
    scope: &crate::harness::account_work::Scope,
    materialize: impl FnOnce(AdmittedGrant<'_>) -> Result<(R, slot::MaterializationReceipt), String>,
) -> Result<(R, slot::Revision), OwnerRefreshError> {
    with_refresh_work_scoped(env, scope, None, |source| {
        with_refreshed_owner_held_work(env, binding, caller, request, materialize, source, None)
    })
}

pub(in crate::harness) fn with_refreshed_owner_in_namespace<R>(
    env: &Env,
    binding: &GrantBinding,
    caller: &slot::Caller,
    request: &slot::RefreshRequest,
    namespace: &slot::RefreshNamespace,
    materialize: impl FnOnce(AdmittedGrant<'_>) -> Result<(R, slot::MaterializationReceipt), String>,
) -> Result<(R, slot::Revision), OwnerRefreshError> {
    let operation = delivery_operation(binding, caller, request)?;
    current_source(env, binding, &operation, Some(namespace))
        .map_err(|_| OwnerRefreshError::Unavailable)?;
    let borrowed = namespace
        .source_lease()
        .map_err(|_| OwnerRefreshError::Unavailable)?;
    with_refresh_work_scoped(env, namespace.scope(), borrowed, |source| {
        with_refreshed_owner_held_work(
            env,
            binding,
            caller,
            request,
            materialize,
            source,
            Some(namespace),
        )
    })
}

fn with_refreshed_owner_held_work<R>(
    env: &Env,
    binding: &GrantBinding,
    caller: &slot::Caller,
    request: &slot::RefreshRequest,
    materialize: impl FnOnce(AdmittedGrant<'_>) -> Result<(R, slot::MaterializationReceipt), String>,
    source: &RefreshWorkLease,
    namespace: Option<&slot::RefreshNamespace>,
) -> Result<(R, slot::Revision), OwnerRefreshError> {
    let deadline = crate::harness::auth_deadline::Deadline::after(REFRESH_BUDGET);
    let operation = delivery_operation(binding, caller, request)?;
    let mut materialize = Some(materialize);
    for _ in 0..3 {
        source
            .current()
            .map_err(|_| OwnerRefreshError::Unavailable)?;
        reconcile_current_held_work(env, binding, caller, source)?;
        current_source(env, binding, &operation, namespace)
            .map_err(|_| OwnerRefreshError::Unavailable)?;
        let lineage = source
            .send_lineage(env, binding)
            .map_err(|_| OwnerRefreshError::Unavailable)?;
        match prepare(env, binding, caller, &operation, &lineage, namespace)? {
            Preparation::Reconcile => continue,
            Preparation::Ready => {
                return deliver(
                    env,
                    binding,
                    caller,
                    &operation,
                    namespace,
                    materialize.take().expect("materializer consumed once"),
                );
            }
            Preparation::Exchange(prepared) => {
                let progress =
                    exchange_prepared(env, binding, prepared, source, &lineage, deadline, || {
                        current_source(env, binding, &operation, namespace)
                    })?;
                drop(lineage);
                match progress {
                    ExchangeProgress::Responded => {
                        reconcile_current_held_work(env, binding, caller, source)?;
                    }
                    ExchangeProgress::NotSent => {
                        reconcile_current_held_work(env, binding, caller, source)?;
                        return Err(OwnerRefreshError::Unavailable);
                    }
                    ExchangeProgress::Unknown => return Err(OwnerRefreshError::Unavailable),
                }
            }
        }
    }
    Err(OwnerRefreshError::Unavailable)
}

pub(super) enum ExchangeProgress {
    Responded,
    NotSent,
    Unknown,
}

pub(super) fn exchange_prepared(
    env: &Env,
    binding: &GrantBinding,
    prepared: Box<PreparedExchange>,
    source: &RefreshWorkLease,
    lineage: &RefreshLineageLease,
    deadline: crate::harness::auth_deadline::Deadline,
    before_send: impl FnOnce() -> Result<(), String>,
) -> Result<ExchangeProgress, OwnerRefreshError> {
    let PreparedExchange {
        claimed,
        witness,
        document,
    } = *prepared;
    source
        .current()
        .map_err(|_| OwnerRefreshError::Unavailable)?;
    source
        .provider_current()
        .map_err(|_| OwnerRefreshError::Unavailable)?;
    let claimed = claimed
        .arm_for_send(witness.clone())
        .map_err(OwnerRefreshError::Retaining)?;
    if source.current().is_err() {
        drop(
            claimed
                .mark_not_sent(witness)
                .map_err(OwnerRefreshError::Retaining)?,
        );
        return Ok(ExchangeProgress::NotSent);
    }
    match codex_app_login::exchange_owned_refresh(document.as_slice(), deadline, || {
        source.provider_current()?;
        let current = claimed.view(&witness)?;
        lineage.matches(
            binding,
            &current.identity,
            &current.revision,
            document.as_slice(),
        )?;
        lineage.check_catalogue(env, binding, &current.identity, document.as_slice())?;
        before_send()
    }) {
        Ok(codex_app_login::RefreshExchange::Completed(response)) => {
            drop(
                claimed
                    .retain_received(response, witness)
                    .map_err(OwnerRefreshError::Retaining)?,
            );
            Ok(ExchangeProgress::Responded)
        }
        Ok(codex_app_login::RefreshExchange::NotSent) | Err(_) => {
            drop(
                claimed
                    .mark_not_sent(witness)
                    .map_err(OwnerRefreshError::Retaining)?,
            );
            Ok(ExchangeProgress::NotSent)
        }
        Ok(codex_app_login::RefreshExchange::Unknown) => {
            drop(claimed); // durable Attempted forbids reuse; no invented NoSend.
            Ok(ExchangeProgress::Unknown)
        }
    }
}

/// Test-only received-response promotion through the same canonical C2 writer.
#[cfg(all(test, unix))]
pub(in crate::harness) fn fixture_promote_received_response(
    env: &crate::harness::cli::Env,
    binding: &super::GrantBinding,
    scope: &crate::harness::account_work::Scope,
) {
    tests::inject_received_response(env, binding, scope);
    assert!(reconcile_retained(env, binding, scope).is_ok());
}

/// Synthetic received boundary only; preserve the real encrypted pending
/// response and canonical OwnerAttempt for hostile pre-use inventory controls.
#[cfg(all(test, unix))]
pub(super) fn fixture_receive_lineage_response(
    env: &Env,
    binding: &GrantBinding,
    scope: &crate::harness::account_work::Scope,
    refresh: &str,
) {
    let operation = aterm_uds::rand::hex_token::<16>().unwrap();
    tests::inject_lineage_response(env, binding, scope, Some(operation), refresh);
}

/// Synthetic received boundary followed by real C2 encryption/CAS, stopped
/// only after that durable boundary. This does not construct an OwnerWitness.
#[cfg(all(test, unix))]
pub(super) fn fixture_leave_anchored_publishing(
    env: &Env,
    binding: &GrantBinding,
    scope: &crate::harness::account_work::Scope,
) {
    fixture_receive_lineage_response(env, binding, scope, "cold-publishing-candidate");
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            STOP_AFTER_PUBLISHING_ANCHOR.set(self.0);
        }
    }
    let _reset = Reset(STOP_AFTER_PUBLISHING_ANCHOR.replace(true));
    assert!(matches!(
        reconcile_retained(env, binding, scope),
        Err(OwnerRefreshError::Unavailable)
    ));
    let store = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .unwrap();
    let (logins, None) = store.load_logins(env.now).unwrap() else {
        panic!("clean fixture");
    };
    let witness = logins.enrollment.owner_witness(binding).unwrap().unwrap();
    assert_eq!(witness.revision.generation, 2);
    assert!(witness.pending.is_none());
    let claimed = Claimed::open(env, binding, false).unwrap().unwrap();
    claimed.view(&witness).unwrap();
    assert!(claimed.journal.as_ref().unwrap().pending.as_ref().is_some_and(|pending|
        pending.phase == Phase::Publishing && pending.response.is_some()));
}

/// Synthetic received boundary, real encrypted/canonical promotion. No HTTP or
/// source/delivery permission is minted by this private-world test helper.
#[cfg(all(test, unix))]
pub(in crate::harness) fn fixture_promote_lineage_response(
    env: &Env,
    binding: &GrantBinding,
    scope: &crate::harness::account_work::Scope,
    refresh: &str,
) {
    let mut nonce = [0u8; 16];
    aterm_uds::rand::fill(&mut nonce).unwrap();
    let operation = aterm_codec::hex::encode_lower(&nonce);
    nonce.zeroize();
    tests::inject_lineage_response(env, binding, scope, Some(operation), refresh);
    assert!(reconcile_retained(env, binding, scope).is_ok());
}
#[cfg(all(test, unix))]
#[path = "codex_grant_refresh_transaction/native_wait_tests.rs"]
mod native_wait_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn exchange_file(env: &Env) -> std::fs::File {
        let namespace = crate::harness::codex_source_gate::existing_namespace_for_home(
            env.home.as_deref().unwrap(),
        )
        .unwrap();
        private_file::open_private_rw(namespace.directory().unwrap(), "native-code-exchange.lock")
            .unwrap()
    }

    #[cfg(unix)]
    #[expect(
        clippy::disallowed_methods,
        reason = "test the actual kernel exclusion used by native refresh"
    )]
    fn exchange_is_exclusive(file: &std::fs::File) -> bool {
        match file.try_lock() {
            Ok(()) => true,
            Err(std::fs::TryLockError::WouldBlock) => false,
            Err(std::fs::TryLockError::Error(error)) => panic!("kernel lock failed: {error}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_exchange_exclusion_precedes_owner_work_and_releases_after_completion() {
        // Real kernel locks only; this fixture grants no credential or HTTP authority.
        let fixture = super::super::super::maintenance_tests::fixture();
        let lease = RefreshWorkLease::for_env(&fixture.env).unwrap();
        let file = exchange_file(&fixture.env);
        assert!(!exchange_is_exclusive(&file));
        drop(lease);
        assert!(exchange_is_exclusive(&file));
        let mut entered = false;
        let result = with_refresh_work(&fixture.env, |_| {
            entered = true;
            Ok(())
        });
        assert!(matches!(result, Err(OwnerRefreshError::Unavailable)));
        assert!(!entered, "exchange contention must precede owner work");
        drop(file);
        with_refresh_work(&fixture.env, |work| {
            work.current().unwrap();
            assert!(!exchange_is_exclusive(&exchange_file(&fixture.env)));
            Ok(())
        })
        .unwrap();
        assert!(exchange_is_exclusive(&exchange_file(&fixture.env)));
    }

    #[cfg(unix)]
    #[test]
    fn legacy_signin_excludes_refresh_before_owner_work() {
        let fixture = super::super::super::maintenance_tests::fixture();
        let store = fixture
            .env
            .accounts_store_for(crate::harness::accounts::Cli::Codex)
            .unwrap();
        let signin = store.signin_now().unwrap().unwrap();
        let mut entered = false;
        let result = with_refresh_work(&fixture.env, |_| {
            entered = true;
            Ok(())
        });
        assert!(matches!(result, Err(OwnerRefreshError::Unavailable)));
        assert!(
            !entered,
            "legacy sign-in must exclude refresh before owner work"
        );
        let publication = crate::harness::codex_source_gate::PublicationLock::for_home(
            fixture.env.home.as_deref().unwrap(),
        )
        .expect("failed refresh admission must release its source lease");
        drop(publication);
        drop(signin);
        with_refresh_work(&fixture.env, |work| {
            work.current().unwrap();
            Ok(())
        })
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn uncertain_retention_and_continuation_keep_the_original_exchange_lease() {
        // Force an actual encrypted-store failure. This tests custody plumbing,
        // not a provider response or a qualified process/source census.
        let fixture = super::super::super::maintenance_tests::fixture();
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let claimed = super::super::super::with_maintenance_owner(
            &fixture.env,
            &fixture.binding,
            &scope,
            |_| {
                Claimed::open(&fixture.env, &fixture.binding, false)?.ok_or("journal absent".into())
            },
        )
        .unwrap();
        let directory = claimed.directory.clone();
        let held_directory = directory.with_extension("retention-test-held");
        let result = with_refresh_work::<()>(&fixture.env, |_| {
            std::fs::rename(&directory, &held_directory).unwrap();
            let Err(retention) = claimed.persist() else {
                panic!("moved store must refuse the write");
            };
            Err(OwnerRefreshError::Retaining(retention))
        });
        let Err(OwnerRefreshError::Retaining(retention)) = result else {
            panic!("uncertain write lost its owned retention");
        };
        assert!(!exchange_is_exclusive(&exchange_file(&fixture.env)));
        std::fs::rename(&held_directory, &directory).unwrap();
        let continuation = retention.retry().unwrap();
        assert!(!exchange_is_exclusive(&exchange_file(&fixture.env)));
        continuation.reconcile(&fixture.env, &scope).unwrap();
        assert!(exchange_is_exclusive(&exchange_file(&fixture.env)));
    }

    #[cfg(unix)]
    #[test]
    fn completed_raw_retains_original_scope_through_end_and_paused_no_caller_settlement() {
        completed_raw_after_end(false);
    }

    #[cfg(unix)]
    #[test]
    fn explicit_hold_defers_durable_raw_without_pinning_ended_source_scope() {
        completed_raw_after_end(true);
    }

    #[cfg(unix)]
    fn completed_raw_after_end(held: bool) {
        let fixture = super::super::super::maintenance_tests::fixture();
        completed_raw_in(&fixture.env, &fixture.binding, &fixture.auth, held, false);
    }

    #[cfg(unix)]
    #[test]
    fn global_held_raw_deferral_retires_actual_principal_and_token_resources() {
        let fixture =
            crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"b".repeat(64));
        fixture.retire_originals().unwrap();
        let status = fixture.publish_first().unwrap();
        assert_eq!(
            status.phase,
            crate::harness::codex_enrollment::Phase::Applied
        );
        let env = fixture.env();
        let store = env
            .accounts_store_for(crate::harness::accounts::Cli::Codex)
            .unwrap();
        let (logins, warning) = store.load_logins(env.now).unwrap();
        assert!(warning.is_none());
        let request = &fixture.requests()[0];
        let binding = logins
            .enrollment
            .published_grant(&request.label, status.generation.unwrap())
            .unwrap();
        let auth = logins
            .enrollment
            .catalogue_staging(request)
            .unwrap()
            .directory
            .join("auth.json");
        completed_raw_in(env, &binding, &auth, true, true);
    }

    #[cfg(unix)]
    fn completed_raw_in(env: &Env, binding: &GrantBinding, auth: &Path, held: bool, global: bool) {
        let lifetime = crate::harness::account_work::Lifetime::default();
        let source_use = lifetime.enter().unwrap();
        let worker = lifetime.enter().unwrap();
        let source_status = source_use.cancellation();
        inject_received_response(env, binding, &source_use);
        let claimed =
            super::super::super::with_maintenance_owner(env, binding, &source_use, |_| {
                Claimed::open(env, binding, false)?.ok_or("owned response missing".into())
            })
            .unwrap();
        let resource_probe = global.then(|| {
            super::super::super::response_resource_identity(
                &binding.request,
                &codex_app_login::renewal_identity(auth.parent().unwrap()).unwrap(),
                &std::fs::read(auth).unwrap(),
            )
            .unwrap()
        });
        let directory = claimed.directory.clone();
        let displaced = directory.with_extension("counted-raw-held");
        let failed = with_refresh_work_scoped::<()>(env, &source_use, None, |work| {
            work.provider_current().unwrap();
            let lineage = work.reconcile_lineage(env, binding).unwrap();
            assert_eq!(lineage.is_some(), global);
            drop(lineage); // The Work retains the real resource claim.
            if let Some(proof) = &resource_probe {
                let namespace = crate::harness::codex_source_gate::existing_namespace_for_home(
                    env.home.as_deref().unwrap(),
                )
                .unwrap();
                assert!(
                    crate::harness::native_enrollment_admission::RefreshResourceTurn::try_acquire(
                        namespace, proof
                    )
                    .unwrap()
                    .is_none()
                );
            }
            std::fs::rename(&directory, &displaced).unwrap();
            let Err(retention) = claimed.persist() else {
                panic!("fixture write must fail");
            };
            Err(OwnerRefreshError::Retaining(retention))
        });
        let Err(OwnerRefreshError::Retaining(retention)) = failed else {
            panic!("owned raw response was erased");
        };
        source_status.cancel();
        drop(source_use);
        assert!(
            !source_status.retired(),
            "End cannot retire the actual source operation while raw custody remains"
        );
        let pause = lifetime.pause().unwrap();
        let pause = match pause.try_wait() {
            Err(pause) => pause,
            Ok(_) => panic!("raw and worker operations still count"),
        };
        let work = retention.work_lease.as_ref().unwrap();
        assert!(
            work.provider_current().is_err(),
            "revoked source cannot mint a new provider request"
        );
        work.current().unwrap();
        std::fs::rename(&displaced, &directory).unwrap();
        let continuation = retention.retry().unwrap();
        assert!(!source_status.retired());
        // The caller PID/Slot is never consulted. The genuine captured source
        // scope, rather than the worker's separate operation, counts promotion.
        let roster = env
            .accounts_roster_for(crate::harness::accounts::Cli::Codex)
            .unwrap();
        let original_roster = std::fs::read(&roster).unwrap();
        let original_auth = std::fs::read(auth).unwrap();
        if held {
            let mut held_roster = crate::harness::accounts::Roster::parse_for(
                crate::harness::accounts::Cli::Codex,
                std::str::from_utf8(&original_roster).unwrap(),
            )
            .unwrap();
            held_roster
                .rows
                .iter_mut()
                .find(|row| row.label == binding.request.label)
                .unwrap()
                .held_by = Some("explicit-security-hold".into());
            let held_text = held_roster
                .rows
                .iter()
                .map(|row| crate::harness::accounts::row_text(row).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(&roster, held_text).unwrap();
            assert!(matches!(
                continuation.reconcile(env, &worker),
                Err(OwnerRefreshError::Unavailable)
            ));
            assert_eq!(std::fs::read(auth).unwrap(), original_auth);
            let store = env
                .accounts_store_for(crate::harness::accounts::Cli::Codex)
                .unwrap();
            let (logins, warning) = store.load_logins(env.now).unwrap();
            assert!(warning.is_none());
            let witness = logins.enrollment.owner_witness(binding).unwrap().unwrap();
            assert_eq!(witness.revision.generation, 1);
            assert!(witness.pending.is_some());
            let claimed = Claimed::open(env, binding, false).unwrap().unwrap();
            assert!(matches!(
                claimed
                    .journal
                    .as_ref()
                    .unwrap()
                    .pending
                    .as_ref()
                    .unwrap()
                    .phase,
                Phase::Responded
            ));
            drop(claimed);
        } else {
            continuation.reconcile(env, &worker).unwrap();
        }
        assert!(source_status.retired());
        assert!(exchange_is_exclusive(&exchange_file(env)));
        let publication = crate::harness::codex_source_gate::PublicationLock::for_home(
            env.home.as_deref().unwrap(),
        )
        .expect("settled or durably deferred response must release source exclusion");
        drop(publication);
        if let Some(proof) = &resource_probe {
            let namespace = crate::harness::codex_source_gate::existing_namespace_for_home(
                env.home.as_deref().unwrap(),
            )
            .unwrap();
            let released =
                crate::harness::native_enrollment_admission::RefreshResourceTurn::try_acquire(
                    namespace, proof,
                )
                .unwrap();
            assert!(
                released.is_some(),
                "no principal/actual-token claim may outlive the retired source count"
            );
            drop(released);
        }
        assert!(worker.cancelled());
        drop(worker);
        assert!(pause.try_wait().is_ok());
        if held {
            // Explicit hold never grants delivery or cache mutation. A later
            // actual counted maintenance operation may settle the same durable
            // response after the hold is removed; no second HTTP send occurs.
            std::fs::write(&roster, original_roster).unwrap();
            let maintenance_lifetime = crate::harness::account_work::Lifetime::default();
            let maintenance_scope = maintenance_lifetime.enter().unwrap();
            reconcile_retained(env, binding, &maintenance_scope).unwrap();
        }
        let (logins, warning) = env
            .accounts_store_for(crate::harness::accounts::Cli::Codex)
            .unwrap()
            .load_logins(env.now)
            .unwrap();
        assert!(warning.is_none());
        assert_eq!(
            logins
                .enrollment
                .owner_witness(binding)
                .unwrap()
                .unwrap()
                .revision
                .generation,
            2
        );
    }

    #[cfg(unix)]
    #[test]
    fn confirmed_received_token_alias_is_durable_without_self_pinning_source_end() {
        let fixture =
            crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"b".repeat(64));
        fixture.retire_originals().unwrap();
        let applied_a = fixture.publish_first().unwrap();
        let env = fixture.env();
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let (work, _) = crate::harness::codex_enrollment::owned_work(
            fixture.requests()[1].clone(),
            scope.clone(),
        );
        let applied_b = fixture.run_recovery(work).unwrap();
        assert_eq!(
            applied_b.phase,
            crate::harness::codex_enrollment::Phase::Applied
        );
        let store = env
            .accounts_store_for(crate::harness::accounts::Cli::Codex)
            .unwrap();
        let (logins, warning) = store.load_logins(env.now).unwrap();
        assert!(warning.is_none());
        let a = logins
            .enrollment
            .published_grant(&fixture.requests()[0].label, applied_a.generation.unwrap())
            .unwrap();
        let b = logins
            .enrollment
            .published_grant(&fixture.requests()[1].label, applied_b.generation.unwrap())
            .unwrap();
        fixture_promote_lineage_response(env, &a, &scope, "shared-actual-candidate-fixture");
        inject_lineage_response(env, &b, &scope, None, "shared-actual-candidate-fixture");
        let auth_b = logins
            .enrollment
            .catalogue_staging(&b.request)
            .unwrap()
            .directory
            .join("auth.json");
        let unchanged_b = std::fs::read(&auth_b).unwrap();
        let ended = scope.cancellation();
        ended.cancel();
        assert!(matches!(
            reconcile_retained(env, &b, &scope),
            Err(OwnerRefreshError::Unavailable)
        ));
        let (after, warning) = store.load_logins(env.now).unwrap();
        assert!(warning.is_none());
        let witness = after.enrollment.owner_witness(&b).unwrap().unwrap();
        assert_eq!(witness.revision.generation, 1);
        assert!(witness.pending.is_some());
        let claimed = Claimed::open(env, &b, false).unwrap().unwrap();
        let journal = claimed.journal.as_ref().unwrap();
        assert_eq!(journal.schema, 4);
        assert!(journal.completed.is_empty());
        let pending = journal.pending.as_ref().unwrap();
        assert!(pending.phase == Phase::Quarantined);
        assert!(pending.quarantine_reason == Some(QuarantineReason::KnownAlias));
        assert!(pending.response.is_some());
        drop(claimed);
        assert_eq!(std::fs::read(&auth_b).unwrap(), unchanged_b);
        drop(scope);
        assert!(
            ended.retired(),
            "durable refusal must not retain the ended source's count"
        );
        let next_lifetime = crate::harness::account_work::Lifetime::default();
        let next = next_lifetime.enter().unwrap();
        for binding in [&a, &b] {
            with_refresh_work_scoped(env, &next, None, |work| {
                assert!(
                    work.send_lineage(env, binding).is_err(),
                    "neither overlapping owner may reuse the shared token"
                );
                Ok(())
            })
            .unwrap();
        }
        // These are synthetic received-boundary bodies with real encrypted
        // journals/anchors/resources. No network or provider independence is
        // inferred from this adversarial catalogue control.
    }

    fn operation() -> Operation {
        Operation {
            id: "a".repeat(32),
            caller: slot::Caller {
                sid: "s-fixture".into(),
                pid: 123,
                birth: 456,
            },
            slot: "b".repeat(64),
            expected_digest: "c".repeat(64),
            assignment_generation: 7,
            source_authority: SourceAuthority::Assignment {
                operation: "d".repeat(32),
            },
            credential_generation: 11,
        }
    }

    /// Synthetic received-boundary input; real owner/anchor/journal writes.
    /// No network, Slot, Caller admission or provider-origin proof is minted.
    #[cfg(unix)]
    pub(super) fn inject_received_response(
        env: &crate::harness::cli::Env,
        binding: &super::super::GrantBinding,
        scope: &crate::harness::account_work::Scope,
    ) {
        // This generic promotion fixture models distinct issuer bundles. Alias
        // controls use inject_lineage_response with an explicit shared token.
        let refresh = format!("rotated-refresh-only-{}", binding.request.id);
        inject_lineage_response(env, binding, scope, None, &refresh)
    }
    #[cfg(unix)]
    pub(super) fn inject_lineage_response(
        env: &crate::harness::cli::Env,
        binding: &super::super::GrantBinding,
        scope: &crate::harness::account_work::Scope,
        operation_id: Option<String>,
        refresh: &str,
    ) {
        super::super::super::with_maintenance_owner(
            env, binding, scope, |mut grant| {
                let mut witness = grant.anchor().read()?.ok_or("fixture witness missing")?;
                let mut claimed = Claimed::open(env, binding, false)?
                    .ok_or("fixture journal missing")?;
                let current = claimed.view(&witness)?;
                let mut original = operation();
                if let Some(id) = operation_id { original.id = id; }
                original.expected_digest = current.revision.digest;
                original.credential_generation = 1;
                let token_digest = codex_app_login::owned_refresh_token_digest(
                    current.document.as_slice())?;
                let journal = claimed.journal.as_mut().ok_or("fixture journal missing")?;
                journal.pending = Some(Pending {
                    operation: original.into(), phase: Phase::Prepared, quarantine_reason: None,
                    base_generation: journal.current.generation,
                    base_digest: journal.current.digest.clone(),
                    token_digest: Some(token_digest), response: None,
                });
                journal.transition = JournalTransition {
                    expected: Some(witness), kind: TransitionKind::Attempt,
                };
                claimed = claimed.persist().map_err(|_| "fixture reservation write failed")?;
                grant.anchor().compare_exchange(&claimed.transition()?)?;
                witness = grant.anchor().read()?.ok_or("fixture reservation missing")?;
                claimed = claimed.arm_for_send(witness)
                    .map_err(|_| "fixture attempted write failed")?;
                grant.anchor().compare_exchange(&claimed.transition()?)?;
                witness = grant.anchor().read()?.ok_or("fixture attempt missing")?;
                // Inject bytes at the received-response boundary: no network
                // function is called by this regression or by reconciliation.
                let journal = claimed.journal.as_mut().ok_or("fixture journal missing")?;
                let pending = journal.pending.as_mut().ok_or("fixture attempt missing")?;
                pending.phase = Phase::Responded;
                pending.response = Some(Response {
                    status: 200, received_at: Some(env.now as u64),
                    body: aterm_codec::base64::encode(&aterm_json::to_vec(&aterm_json::json!({"access_token":"fresh-access-only", "refresh_token":refresh})).map_err(|_| "fixture encoding failed")?)
                        .map_err(|_| "fixture encoding failed")?,
                });
                journal.transition = JournalTransition {
                    expected: Some(witness), kind: TransitionKind::Response,
                };
                claimed = claimed.persist().map_err(|_| "fixture response write failed")?;
                grant.anchor().compare_exchange(&claimed.transition()?)?;
                Ok(())
            },
        ).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn cancelled_sender_durable_response_promotes_shared_owner_once_without_delivery() {
        // Synthetic provider completion; all account, issuer-seal, encrypted
        // journal and canonical generation/witness paths are real. This does
        // not qualify OAuth or the prospective source-publication barrier.
        let fixture = super::super::super::maintenance_tests::fixture();
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        inject_received_response(&fixture.env, &fixture.binding, &scope);
        scope.cancellation().cancel();
        assert!(scope.cancelled());
        for _ in 0..2 {
            assert!(reconcile_retained(&fixture.env, &fixture.binding, &scope).is_ok());
        }
        super::super::super::with_maintenance_owner(
            &fixture.env,
            &fixture.binding,
            &scope,
            |mut grant| {
                let witness = grant.anchor().read()?.ok_or("settled witness missing")?;
                assert_eq!(witness.revision.generation, 2);
                assert!(witness.pending.is_none());
                let claimed = Claimed::open(&fixture.env, &fixture.binding, false)?
                    .ok_or("settled journal missing")?;
                let current = claimed.view(&witness)?;
                assert_eq!(current.revision.generation, 2);
                assert!(std::fs::read(&fixture.auth).unwrap() == current.document.as_slice());
                let journal = claimed.journal.as_ref().unwrap();
                assert!(journal.pending.is_none());
                assert!(journal.previous_current.is_none());
                assert_eq!(journal.completed.len(), 1);
                assert_eq!(journal.completed[0].operation.id(), "a".repeat(32));
                assert!(journal.completed[0].spent_token_digest.is_some());
                Ok(())
            },
        )
        .unwrap();
        let store = fixture
            .env
            .accounts_store_for(crate::harness::accounts::Cli::Codex)
            .unwrap();
        assert!(!store.dir.join("session-slots").exists());
        assert!(!scope.cancellation().retired());
    }

    #[test]
    fn immutable_delivery_digest_binds_every_caller_assignment_and_request_field() {
        let original = operation();
        let expected = operation_digest(&original).unwrap();
        for field in 0..8 {
            let mut changed = original.clone();
            match field {
                0 => changed.id = "e".repeat(32),
                1 => changed.caller.sid = "s-other".into(),
                2 => changed.caller.pid += 1,
                3 => changed.caller.birth += 1,
                4 => changed.slot = "e".repeat(64),
                5 => changed.expected_digest = "e".repeat(64),
                6 => changed.assignment_generation += 1,
                _ => {
                    changed.source_authority = SourceAuthority::Assignment {
                        operation: "e".repeat(32),
                    }
                }
            }
            assert_ne!(operation_digest(&changed).unwrap(), expected);
            assert!(!same_wire_operation(&original, &changed));
        }
        let mut observed = original.clone();
        observed.credential_generation += 1;
        assert!(
            same_wire_operation(&original, &observed),
            "lost ACK can advance the authenticated slot fact"
        );
        assert_ne!(
            operation_digest(&observed).unwrap(),
            expected,
            "the recorded attempt still binds its original fact"
        );
    }

    #[test]
    fn startup_refresh_correlation_is_distinct_from_an_assignment_intent() {
        let mut startup = operation();
        startup.assignment_generation = 0;
        assert!(!startup.valid());
        startup.source_authority = SourceAuthority::Startup {
            nonce: "d".repeat(32),
        };
        assert!(startup.valid());
        assert!(!same_wire_operation(&operation(), &startup));
        assert_ne!(
            operation_digest(&operation()).unwrap(),
            operation_digest(&startup).unwrap()
        );
        let mut changed = startup.clone();
        changed.source_authority = SourceAuthority::Startup {
            nonce: "e".repeat(32),
        };
        assert!(!same_wire_operation(&startup, &changed));
        changed.credential_generation = 0;
        assert!(!changed.valid());
    }

    #[test]
    #[cfg(unix)]
    fn missing_assignment_book_refuses_the_actual_refresh_send_for_both_source_modes() {
        let fixture = super::super::super::maintenance_tests::fixture();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let document = aterm_json::to_vec(&aterm_json::json!({
            "auth_mode":"chatgpt","tokens":{
                "access_token":"synthetic-source-guard-access",
                "refresh_token":"synthetic-source-guard-refresh",
                "account_id":"synthetic-source-guard-account",
                "oauth_provenance":{
                    "issuer":format!("http://{}",listener.local_addr().unwrap()),
                    "client_id":codex_app_login::CLIENT_ID
                }
            }
        }))
        .unwrap();
        for startup in [false, true] {
            let mut operation = operation();
            if startup {
                operation.assignment_generation = 0;
                operation.source_authority = SourceAuthority::Startup {
                    nonce: "d".repeat(32),
                };
            }
            assert!(operation.valid());
            let mut checked = false;
            let result = codex_app_login::exchange_owned_refresh(
                &document,
                crate::harness::auth_deadline::Deadline::after(aterm_spec::wait::MUST_HAPPEN),
                || {
                    checked = true;
                    current_source(&fixture.env, &fixture.binding, &operation, None)
                },
            );
            assert!(checked, "the real pre-send admission was reached");
            assert!(result.is_err());
            assert_eq!(
                aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Ready,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .unwrap_err()
                .kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
}
