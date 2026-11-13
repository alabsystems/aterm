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
        if phase == Some(Phase::Rejected) {
            let response = claimed
                .journal
                .as_ref()
                .and_then(|journal| journal.pending.as_ref())
                .and_then(|pending| pending.response.as_ref())
                .ok_or("rejected owner response absent")?;
            let body = response.bytes()?;
            let Some(settled_at) = codex_app_login::owned_refresh_refusal_settled_at(
                response.status,
                body.as_slice(),
                response.received_at,
                env.now,
            ) else {
                // Keep the maintenance ticket paced until the same credential has outlived
                // every permitted rotation publication. The terminal Rejected lineage no
                // longer blocks other grants while this grant waits for that local proof.
                return Err("refresh refusal settlement pending".into());
            };
            let settlement = anchor.record_settled_refusal(&witness, settled_at)?;
            anchor.retire_catalogue(env, &settlement)?;
            changed = true;
            return Ok(changed);
        }
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
                        // A refusal for good issued no token, aliased grant or not: the
                        // exchange's outcome is known, so the grant is rejected and no lineage
                        // is in doubt (`lineage_history`). Any other answer it cannot use is
                        // quarantined as uncertain.
                        let refused =
                            codex_app_login::owned_refresh_refused(response.status, raw.as_slice());
                        journal.pending.as_mut().expect("checked pending").phase = if refused {
                            Phase::Rejected
                        } else {
                            Phase::Quarantined
                        };
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

/// Terminally retain an answer that cannot be interpreted. This strengthens `Responded` into a
/// principal-confined quarantine: any unknown successor remains fenced by the authenticated
/// principal, while unrelated grants no longer wait. It cannot publish a token, reject a grant or
/// send HTTP.
fn quarantine_uncertain_response(
    env: &Env,
    binding: &GrantBinding,
    anchor: &mut codex_enrollment::OwnerAnchorWriter<'_>,
    retained: &mut Option<Box<OwnedRefreshRetention>>,
) -> Result<bool, String> {
    let witness = anchor.read()?.ok_or("canonical owner anchor absent")?;
    let mut claimed = Claimed::open(env, binding, false)?.ok_or("owner journal absent")?;
    claimed.view(&witness)?;
    let journal = claimed.journal.as_mut().ok_or("owner journal absent")?;
    let pending = match journal.pending.as_ref() {
        Some(pending) if pending.phase == Phase::Responded => pending,
        _ => return Ok(false),
    };
    let response = pending.response.as_ref().ok_or("owner response absent")?;
    let body = response.bytes()?;
    let base = journal.current.open()?;
    let mergeable = response.received_at.is_some_and(|received| {
        codex_app_login::merge_owned_refresh(
            base.document.as_slice(),
            response.status,
            body.as_slice(),
            received,
        )
        .is_ok()
    });
    if mergeable || codex_app_login::owned_refresh_refused(response.status, body.as_slice()) {
        return Ok(false);
    }
    journal.pending.as_mut().expect("checked response").phase = Phase::Quarantined;
    journal.transition = JournalTransition {
        expected: Some(witness),
        kind: TransitionKind::Response,
    };
    claimed = persist_owned(claimed, retained)?;
    anchor.compare_exchange(&claimed.transition()?)?;
    Ok(true)
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
    let lineage = match source.reconcile_lineage(env, binding) {
        Ok(lineage) => lineage,
        Err(_) => {
            let mut retained = None;
            let result = super::super::with_maintenance_owner(env, binding, scope, |mut grant| {
                quarantine_uncertain_response(env, binding, grant.anchor(), &mut retained).and_then(
                    |changed| {
                        changed
                            .then_some(())
                            .ok_or_else(|| "refresh lineage reconciliation pending".to_owned())
                    },
                )
            });
            if let Some(retained) = retained {
                return Err(OwnerRefreshError::Retaining(retained));
            }
            source
                .current()
                .map_err(|_| OwnerRefreshError::Unavailable)?;
            return result.map_err(|_| OwnerRefreshError::Unavailable);
        }
    };
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
#[path = "codex_grant_refresh_transaction_tests.rs"]
mod tests;
