// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Natural account reads use the canonical owner, never a fabricated session.
//! A retained response has its own app-owned, no-HTTP settlement continuation.
use super::lineage::RefreshLineageLease;
use super::*;
use crate::harness::{account_work, accounts, accounts_pool, codex_source_gate, provider_read};
use std::sync::Arc;
use std::time::{Duration, Instant};

// Closed test diagnostics only: no error text, paths, principals or credentials.
#[cfg(all(test, unix))]
std::thread_local! {
    static NATIVE_USAGE_TEST_STAGE: std::cell::Cell<&'static str> = const { std::cell::Cell::new("not_started") };
}
#[cfg(all(test, unix))]
pub(super) fn test_stage(stage: &'static str) {
    NATIVE_USAGE_TEST_STAGE.set(stage);
}

#[derive(Clone)]
pub struct NativeUsageSourceLease {
    pub(in crate::harness) inner: Arc<codex_source_gate::SourceWorkLease>,
}
impl std::fmt::Debug for NativeUsageSourceLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeUsageSourceLease(kernel-only)")
    }
}

pub struct NativeUsageRequest<'a> {
    pub env: &'a Env,
    pub home: &'a Path,
    pub expected_email: Option<&'a str>,
    pub now: i64,
    pub wait: Duration,
    pub scope: &'a account_work::Scope,
    pub use_token: provider_read::Use,
    pub source: Option<NativeUsageSourceLease>,
}

#[must_use = "completed native responses must remain owned until storage settles"]
pub enum NativeUsageOutcome {
    Observed(accounts_pool::UsageObservation),
    Retaining(NativeUsageRetention),
}
#[must_use = "a deferred journal must remain discoverable for later maintenance"]
#[derive(Debug)]
pub enum NativeUsageSettlement {
    Settled,
    Deferred(NativeUsageDeferred),
}

struct ReadContext {
    permit: provider_read::Permit,
    env: Env,
    home: PathBuf,
    expected_email: Option<String>,
    deadline: Instant,
    scope: account_work::Scope,
}
struct MaintenanceContext {
    env: Env,
    scope: account_work::Scope,
}
enum SettlementContext {
    Read(Box<ReadContext>),
    Maintenance(Box<MaintenanceContext>),
}
impl SettlementContext {
    fn env(&self) -> &Env {
        match self {
            Self::Read(context) => &context.env,
            Self::Maintenance(context) => &context.env,
        }
    }
    fn scope(&self) -> &account_work::Scope {
        match self {
            Self::Read(context) => &context.scope,
            Self::Maintenance(context) => &context.scope,
        }
    }
}
struct RetentionState {
    inner: Box<OwnedRefreshRetention>,
    context: SettlementContext,
}
struct ContinuationState {
    inner: OwnerRefreshContinuation,
    context: SettlementContext,
}

#[path = "codex_grant_native_usage_discovery.rs"]
mod discovery;
pub use discovery::{NativeUsageDiscoveryCursor, discover_native_usage_deferred};

const MAX_MAINTENANCE_PACING: u8 = 8;

struct DeferredState {
    env: Env,
    binding: GrantBinding,
    owner: account_work::MaintenanceOwner,
    source: Option<codex_source_gate::SourceGateIdentity>,
    epoch: Option<account_work::MaintenanceEpoch>,
    not_before: Option<Instant>,
    failures: u8,
    parked_namespace: bool,
}
/// Only encrypted journal location and weak App admission remain. There is no
/// Scope, provider Permit, plaintext response, or source/kernel claim to revive.
#[must_use = "retain this durable journal locator until later maintenance"]
pub struct NativeUsageDeferred(Box<DeferredState>);
impl std::fmt::Debug for NativeUsageDeferred {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeUsageDeferred(storage-only)")
    }
}
impl NativeUsageDeferred {
    /// Coalescing metadata from two already authenticated tickets, never a
    /// credential-use or App-admission proof.
    pub fn same_journal(&self, other: &Self) -> bool {
        self.0.binding == other.0.binding && self.0.source == other.0.source
    }
    pub fn retained_by(&self, retained: &NativeUsageRetention) -> bool {
        retained
            .0
            .inner
            .claimed
            .journal
            .as_ref()
            .is_some_and(|journal| {
                journal.binding == self.0.binding
                    && retained
                        .0
                        .inner
                        .work_lease
                        .as_ref()
                        .is_some_and(|work| self.0.source.as_ref() == Some(&work.source_identity()))
            })
    }
    /// Admit a genuinely new operation only after the original has retired and
    /// the same App accepts ordinary work. Reconciliation cannot send HTTP or
    /// materialize a consumer source. A pause leaves this metadata unchanged.
    pub fn resume(self) -> Result<NativeUsageSettlement, NativeUsageRetention> {
        self.resume_at(Instant::now())
    }
    fn resume_at(mut self, now: Instant) -> Result<NativeUsageSettlement, NativeUsageRetention> {
        if self.0.parked_namespace {
            return Ok(NativeUsageSettlement::Deferred(self));
        }
        let Some(epoch) = self.0.owner.admission_epoch() else {
            return Ok(NativeUsageSettlement::Deferred(self));
        };
        if self.0.epoch.as_ref() != Some(&epoch) {
            self.0.epoch = Some(epoch);
            self.0.failures = 0;
            self.0.not_before = None;
        }
        if self.0.not_before.is_some_and(|until| now < until) {
            return Ok(NativeUsageSettlement::Deferred(self));
        }
        let Some(scope) = self.0.owner.enter() else {
            return Ok(NativeUsageSettlement::Deferred(self));
        };
        self.0.owner = scope.maintenance_owner();
        self.0.epoch = Some(scope.maintenance_epoch());
        if self
            .0
            .source
            .as_ref()
            .is_none_or(|source| !source.current())
        {
            self.0.parked_namespace = true;
            return Ok(NativeUsageSettlement::Deferred(self));
        }
        self.0.env.now = crate::supervise::limit::unix_now().max(self.0.env.now);
        let state = *self.0;
        let DeferredState {
            env,
            binding,
            owner,
            source,
            epoch,
            not_before: _,
            failures,
            parked_namespace: _,
        } = state;
        let context = SettlementContext::Maintenance(Box::new(MaintenanceContext { env, scope }));
        let result = transaction::reconcile_retained_in_source(
            context.env(),
            &binding,
            context.scope(),
            source.as_ref().expect("checked actual source identity"),
        );
        match result {
            Ok(()) => Ok(NativeUsageSettlement::Settled),
            Err(OwnerRefreshError::Unavailable) => {
                let failures = failures.saturating_add(1).min(MAX_MAINTENANCE_PACING);
                let delay =
                    Duration::from_secs((1u64 << failures.saturating_sub(1).min(6)).min(60));
                let parked_namespace = source.as_ref().is_none_or(|source| !source.current());
                Ok(NativeUsageSettlement::Deferred(Self(Box::new(
                    DeferredState {
                        env: context.env().clone(),
                        binding,
                        owner,
                        source,
                        epoch,
                        not_before: now.checked_add(delay),
                        failures,
                        parked_namespace,
                    },
                ))))
            }
            Err(OwnerRefreshError::Retaining(inner)) => {
                Err(NativeUsageRetention(Box::new(RetentionState {
                    inner,
                    context,
                })))
            }
        }
    }
}
#[must_use = "retry or retain this completed native response"]
pub struct NativeUsageRetention(Box<RetentionState>);
#[must_use = "settle or retain this no-HTTP native continuation"]
pub struct NativeUsageContinuation(Box<ContinuationState>);
impl std::fmt::Debug for NativeUsageRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeUsageRetention(redacted)")
    }
}
impl std::fmt::Debug for NativeUsageContinuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeUsageContinuation(redacted)")
    }
}
impl NativeUsageRetention {
    pub fn scope(&self) -> &account_work::Scope {
        self.0.context.scope()
    }
    /// Storage only: this cannot resend the refresh or perform a quota GET.
    pub fn retry(self) -> Result<NativeUsageContinuation, Self> {
        let RetentionState { inner, context } = *self.0;
        match inner.retry() {
            Ok(inner) => Ok(NativeUsageContinuation(Box::new(ContinuationState {
                inner,
                context,
            }))),
            Err(inner) => Err(Self(Box::new(RetentionState { inner, context }))),
        }
    }
}
impl NativeUsageContinuation {
    pub fn scope(&self) -> &account_work::Scope {
        self.0.context.scope()
    }
    /// Shared promotion or durable deferral only; neither is a quota reading.
    pub fn settle(self) -> Result<NativeUsageSettlement, NativeUsageRetention> {
        let ContinuationState { inner, context } = *self.0;
        let deferred = NativeUsageDeferred(Box::new(DeferredState {
            env: context.env().clone(),
            binding: inner.binding.clone(),
            owner: context.scope().maintenance_owner(),
            source: inner
                .work_lease
                .as_ref()
                .map(|source| source.source_identity()),
            epoch: None,
            not_before: None,
            failures: 0,
            parked_namespace: false,
        }));
        match inner.reconcile(context.env(), context.scope()) {
            Ok(()) => Ok(NativeUsageSettlement::Settled),
            Err(OwnerRefreshError::Unavailable) => Ok(NativeUsageSettlement::Deferred(deferred)),
            Err(OwnerRefreshError::Retaining(inner)) => {
                Err(NativeUsageRetention(Box::new(RetentionState {
                    inner,
                    context,
                })))
            }
        }
    }
}

struct Policy {
    publication: super::super::published_source::PublishedSource,
    binding: GrantBinding,
    auth: PathBuf,
    row_policy: String,
    source: crate::harness::accounts_sync::CodexCompletionSource,
    route: String,
}
impl Policy {
    fn capture(context: &ReadContext, binding: &GrantBinding, auth: &Path) -> Result<Self, String> {
        let store = context
            .env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("native usage store absent")?;
        let roster = context
            .env
            .accounts_roster_for(accounts::Cli::Codex)
            .ok_or("native usage roster absent")?;
        let records = store
            .peek_account_records(&roster, context.env.now)
            .map_err(|_| "native usage policy unavailable")?;
        let row = records
            .roster
            .row(&binding.request.label)
            .ok_or("native usage row absent")?;
        let raw = records
            .raw_roster
            .row(&binding.request.label)
            .ok_or("native usage raw row absent")?;
        let publication = super::super::published_source::PublishedSource::authenticate(
            &context.env,
            binding,
            &records.logins,
            auth,
        )?
        .ok_or("historical usage requires migration")?;
        let source = crate::harness::accounts_sync::codex_completion_source(&context.env, row)?;
        let policy = Self {
            publication,
            binding: binding.clone(),
            auth: auth.into(),
            row_policy: accounts::row_text(raw).ok_or("native usage row policy invalid")?,
            source,
            route: codex_app_login::native_usage_route(
                &context.home,
                context
                    .env
                    .home
                    .as_deref()
                    .ok_or("native usage machine profile absent")?,
            ),
        };
        policy.current(context)?;
        Ok(policy)
    }
    fn current(&self, context: &ReadContext) -> Result<(), String> {
        context.current()?;
        let store = context
            .env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("native usage store absent")?;
        let roster = context
            .env
            .accounts_roster_for(accounts::Cli::Codex)
            .ok_or("native usage roster absent")?;
        let records = store
            .peek_account_records(&roster, context.env.now)
            .map_err(|_| "native usage policy unavailable")?;
        let row = records
            .roster
            .row(&self.binding.request.label)
            .ok_or("native usage row absent")?;
        let raw = records
            .raw_roster
            .row(&self.binding.request.label)
            .ok_or("native usage raw row absent")?;
        let bound = records
            .logins
            .get(&row.label)
            .ok_or("native usage binding absent")?;
        if records
            .logins
            .enrollment
            .published_grant(&row.label, bound.last_success_generation)
            .as_ref()
            != Some(&self.binding)
            || row.held_by.is_some()
            || accounts::row_text(raw).as_deref() != Some(self.row_policy.as_str())
            || crate::harness::accounts_sync::codex_completion_source(&context.env, row)?
                != self.source
            || codex_app_login::native_usage_route(
                &context.home,
                context
                    .env
                    .home
                    .as_deref()
                    .ok_or("native usage machine profile absent")?,
            ) != self.route
        {
            return Err("native usage source policy changed".into());
        }
        self.publication.at(&records.logins, &self.auth)
    }
}
impl ReadContext {
    fn current(&self) -> Result<(), String> {
        if self.scope.cancelled()
            || Instant::now() >= self.deadline
            || !self
                .permit
                .matches(provider_read::Read::CodexEndpoint, &self.home)
        {
            return Err("native usage admission ended".into());
        }
        Ok(())
    }
}

/// Only this module constructs the HTTP reader capability from canonical
/// signed current data, with real natural permission and existing work leases.
pub(in crate::harness) struct NativeUsageGrant<'a> {
    document: SecretBytes,
    identity: Identity,
    revision: slot::Revision,
    binding: GrantBinding,
    policy: Policy,
    context: &'a ReadContext,
    work: &'a transaction::RefreshWorkLease,
    lineage: &'a RefreshLineageLease,
    on_contact: &'a dyn Fn(),
}
impl NativeUsageGrant<'_> {
    pub(in crate::harness) fn document(&self) -> &[u8] {
        self.document.as_slice()
    }
    pub(in crate::harness) fn route(&self) -> &str {
        &self.policy.route
    }
    pub(in crate::harness) fn expected_email(&self) -> Option<&str> {
        self.context.expected_email.as_deref()
    }
    pub(in crate::harness) fn now(&self) -> i64 {
        self.context.env.now
    }
    pub(in crate::harness) fn left(&self) -> Duration {
        self.context
            .deadline
            .saturating_duration_since(Instant::now())
    }
    pub(in crate::harness) fn check(&self) -> Result<(), String> {
        self.context.current()?;
        if !self.work.owns_scope(&self.context.scope) {
            return Err("native usage scope changed".into());
        }
        self.work.provider_current()?;
        self.policy.current(self.context)?;
        self.lineage.matches(
            &self.binding,
            &self.identity,
            &self.revision,
            self.document.as_slice(),
        )?;
        self.lineage.check_catalogue(
            &self.context.env,
            &self.binding,
            &self.identity,
            self.document.as_slice(),
        )
    }
    pub(in crate::harness) fn contact(&self) -> Result<(), String> {
        self.check()?;
        (self.on_contact)();
        self.context.permit.note_contact(self.context.env.now);
        Ok(())
    }
}

fn snapshot<'a>(
    context: &'a ReadContext,
    binding: &GrantBinding,
    work: &'a transaction::RefreshWorkLease,
    lineage: &'a RefreshLineageLease,
    on_contact: &'a dyn Fn(),
) -> Result<NativeUsageGrant<'a>, String> {
    context.current()?;
    if !work.owns_scope(&context.scope) {
        return Err("native usage scope changed".into());
    }
    super::super::with_canonical_owner(
        &context.env,
        binding,
        |grant| {
            super::super::catalogue::require_participant(&context.env, binding, grant.identity)?;
            lineage.check_catalogue(&context.env, binding, grant.identity, grant.document)
        },
        |grant| {
            if grant.canonical_auth.parent() != Some(context.home.as_path()) {
                return Err("native usage home changed".into());
            }
            let result = NativeUsageGrant {
                document: SecretBytes::new(grant.document.to_vec()),
                identity: grant.identity.clone(),
                revision: grant.revision.clone(),
                binding: binding.clone(),
                policy: Policy::capture(context, binding, grant.canonical_auth)?,
                context,
                work,
                lineage,
                on_contact,
            };
            result.check()?;
            Ok(result)
        },
    )
}

fn selected(context: &ReadContext) -> Result<Option<GrantBinding>, String> {
    context.current()?;
    let store = context
        .env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("usage store absent")?;
    let roster = context
        .env
        .accounts_roster_for(accounts::Cli::Codex)
        .ok_or("usage roster absent")?;
    let records = store
        .peek_account_records(&roster, context.env.now)
        .map_err(|_| "usage records unavailable")?;
    let machine = context
        .env
        .home
        .as_deref()
        .ok_or("usage machine profile absent")?;
    let mut selected = None;
    for row in &records.roster.rows {
        let home = row
            .dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| machine.join(".codex"));
        if home != context.home {
            continue;
        }
        if selected.is_some() {
            return Err("usage source ambiguous".into());
        }
        let binding = records.logins.get(&row.label).and_then(|bound| {
            records
                .logins
                .enrollment
                .published_grant(&row.label, bound.last_success_generation)
        });
        if let Some(binding) = binding {
            if !records
                .logins
                .enrollment
                .requires_global_catalogue(&binding.request)?
            {
                return Err("historical usage requires migration".into());
            }
            selected = Some(Some(binding));
        } else if records.logins.enrollment.has_native_label(&row.label) {
            return Err("native usage source is not published".into());
        } else {
            selected = Some(None);
        }
    }
    selected.ok_or_else(|| "usage source is not canonically listed".into())
}

pub fn read_native_usage(request: NativeUsageRequest<'_>) -> NativeUsageOutcome {
    read_native_usage_observed(request, &|| {})
}

/// Observes the first admitted HTTP attempt after final source/policy checks.
/// The borrowed callback cannot escape into storage-only retention.
pub fn read_native_usage_observed(
    request: NativeUsageRequest<'_>,
    on_first_contact: &dyn Fn(),
) -> NativeUsageOutcome {
    #[cfg(all(test, unix))]
    test_stage("admission");
    let contacted = std::cell::Cell::new(false);
    let on_contact = || {
        if !contacted.replace(true) {
            on_first_contact();
        }
    };
    if request.scope.cancelled() {
        return NativeUsageOutcome::Observed(accounts_pool::UsageObservation::unread());
    }
    let Ok(permit) = request.use_token.begin_home(
        provider_read::Read::CodexEndpoint,
        request.home,
        request.now,
    ) else {
        return NativeUsageOutcome::Observed(accounts_pool::UsageObservation::unread());
    };
    let mut env = request.env.clone();
    env.now = request.now;
    let context = ReadContext {
        permit,
        env,
        home: request.home.into(),
        expected_email: request.expected_email.map(str::to_owned),
        deadline: Instant::now()
            .checked_add(request.wait)
            .unwrap_or_else(Instant::now),
        scope: request.scope.clone(),
    };
    #[cfg(all(test, unix))]
    test_stage("selection");
    let binding = match selected(&context) {
        Ok(Some(binding)) => binding,
        Ok(None) => {
            return NativeUsageOutcome::Observed(codex_app_login::native_legacy_observation(
                codex_app_login::LegacyUsageRead {
                    home: &context.home,
                    machine_home: context.env.home.as_deref().unwrap_or(&context.home),
                    wait: request.wait,
                    now: request.now,
                    expected_email: context.expected_email.as_deref(),
                    permit: &context.permit,
                    scope: &context.scope,
                    on_contact: &on_contact,
                },
            ));
        }
        Err(_) => return NativeUsageOutcome::Observed(accounts_pool::UsageObservation::unread()),
    };
    #[cfg(all(test, unix))]
    test_stage("source_exchange_admission");
    let result = transaction::with_refresh_work_scoped(
        &context.env,
        &context.scope,
        request.source.map(|lease| lease.inner),
        |work| native_read(&context, &binding, work, &on_contact),
    );
    match result {
        Ok(observed) => NativeUsageOutcome::Observed(observed),
        Err(OwnerRefreshError::Unavailable) => {
            NativeUsageOutcome::Observed(accounts_pool::UsageObservation::unread())
        }
        Err(OwnerRefreshError::Retaining(inner)) => {
            NativeUsageOutcome::Retaining(NativeUsageRetention(Box::new(RetentionState {
                inner,
                context: SettlementContext::Read(Box::new(context)),
            })))
        }
    }
}

fn native_read(
    context: &ReadContext,
    binding: &GrantBinding,
    work: &transaction::RefreshWorkLease,
    on_contact: &dyn Fn(),
) -> Result<accounts_pool::UsageObservation, OwnerRefreshError> {
    let mut refreshed = false;
    for _ in 0..3 {
        #[cfg(all(test, unix))]
        test_stage("maintenance");
        transaction::reconcile_retained_held_work(&context.env, binding, &context.scope, work)?;
        #[cfg(all(test, unix))]
        test_stage("lineage_admission");
        let lineage = work
            .send_lineage(&context.env, binding)
            .map_err(|_| OwnerRefreshError::Unavailable)?;
        #[cfg(all(test, unix))]
        test_stage("canonical_snapshot");
        let grant = snapshot(context, binding, work, &lineage, on_contact)
            .map_err(|_| OwnerRefreshError::Unavailable)?;
        #[cfg(all(test, unix))]
        test_stage("quota_probe");
        let probe = codex_app_login::native_usage_probe(&grant, !refreshed)
            .ok_or(OwnerRefreshError::Unavailable)?;
        if !probe.proactive && probe.status != Some(401) {
            return Ok(probe.observed);
        }
        if refreshed {
            return Err(OwnerRefreshError::Unavailable);
        }
        let mut nonce = [0u8; 16];
        aterm_uds::rand::fill(&mut nonce).map_err(|_| OwnerRefreshError::Unavailable)?;
        let operation = actor::NativeQuotaOperation {
            id: aterm_codec::hex::encode_lower(&nonce),
            expected_digest: grant.revision.digest.clone(),
            credential_generation: grant.revision.generation,
            native_quota: actor::NativeQuotaContext {
                schema: 1,
                read_context_digest: probe.context_digest,
            },
        };
        nonce.zeroize();
        drop(grant);
        #[cfg(all(test, unix))]
        test_stage("native_prepare");
        match prepare_native(context, binding, work, &lineage, &operation)? {
            NativePreparation::Ready => {
                refreshed = true;
            }
            NativePreparation::Reconcile => {}
            NativePreparation::Exchange(prepared, policy) => {
                let deadline = crate::harness::auth_deadline::Deadline::after(
                    context.deadline.saturating_duration_since(Instant::now()),
                );
                #[cfg(all(test, unix))]
                test_stage("owned_exchange");
                match transaction::exchange_prepared(
                    &context.env,
                    binding,
                    prepared,
                    work,
                    &lineage,
                    deadline,
                    || {
                        policy.current(context)?;
                        on_contact();
                        context.permit.note_contact(context.env.now);
                        Ok(())
                    },
                )? {
                    transaction::ExchangeProgress::Responded => {
                        refreshed = true;
                    }
                    transaction::ExchangeProgress::NotSent
                    | transaction::ExchangeProgress::Unknown => {
                        return Err(OwnerRefreshError::Unavailable);
                    }
                }
            }
        }
        drop(lineage);
    }
    Err(OwnerRefreshError::Unavailable)
}

enum NativePreparation {
    Ready,
    Reconcile,
    Exchange(Box<transaction::PreparedExchange>, Box<Policy>),
}
fn prepare_native(
    context: &ReadContext,
    binding: &GrantBinding,
    work: &transaction::RefreshWorkLease,
    lineage: &RefreshLineageLease,
    operation: &actor::NativeQuotaOperation,
) -> Result<NativePreparation, OwnerRefreshError> {
    let actor = actor::JournalOperation::NativeQuota(operation.clone());
    let mut retained = None;
    let mut result = None;
    let admitted = super::super::with_canonical_owner(
        &context.env,
        binding,
        |grant| {
            context.current()?;
            work.provider_current()?;
            super::super::catalogue::require_participant(&context.env, binding, grant.identity)?;
            lineage.check_catalogue(&context.env, binding, grant.identity, grant.document)
        },
        |mut grant| {
            if !work.owns_scope(&context.scope)
                || grant.canonical_auth.parent() != Some(context.home.as_path())
            {
                return Err("native quota scope changed".into());
            }
            let policy = Policy::capture(context, binding, grant.canonical_auth)?;
            let witness = grant.anchor.read()?.ok_or("native quota witness absent")?;
            let mut claimed = Claimed::open(&context.env, binding, false)?
                .ok_or("native quota journal absent")?;
            let current = claimed.view(&witness)?;
            lineage.matches(
                binding,
                &current.identity,
                &current.revision,
                current.document.as_slice(),
            )?;
            let journal = claimed
                .journal
                .as_mut()
                .ok_or("native quota journal absent")?;
            if let Some(done) = journal
                .completed
                .iter()
                .find(|done| done.operation.id() == actor.id())
            {
                if done.operation != actor {
                    return Err("native quota operation changed".into());
                }
                result = Some(NativePreparation::Ready);
                return Ok(());
            }
            if journal.pending.as_ref().is_some_and(|pending| {
                pending.operation.id() == actor.id() && pending.operation != actor
            }) {
                return Err("native quota operation changed".into());
            }
            if journal.pending.as_ref().is_some_and(|pending| {
                matches!(pending.phase, Phase::Responded | Phase::Publishing)
            }) {
                result = Some(NativePreparation::Reconcile);
                return Ok(());
            }
            if current.revision.generation < operation.credential_generation {
                return Err("native quota revision rolled back".into());
            }
            let needed = if journal
                .pending
                .as_ref()
                .is_some_and(|pending| pending.operation.id() != actor.id())
            {
                2
            } else {
                1
            };
            if journal
                .completed
                .len()
                .checked_add(needed)
                .is_none_or(|count| count > MAX_OPERATIONS)
            {
                return Err("native quota receipt capacity exhausted".into());
            }
            journal.schema = 4;
            if current.revision.digest != operation.expected_digest
                || current.revision.generation > operation.credential_generation
            {
                journal.completed.push(Completed {
                    operation: actor,
                    spent_token_digest: None,
                    generation: current.revision.generation,
                    digest: current.revision.digest,
                });
                journal.transition = JournalTransition {
                    expected: Some(witness),
                    kind: if journal.pending.is_some() {
                        TransitionKind::Attempt
                    } else {
                        TransitionKind::Metadata
                    },
                };
                claimed = transaction::persist_owned(claimed, &mut retained)?;
                grant.anchor.compare_exchange(&claimed.transition()?)?;
                result = Some(NativePreparation::Ready);
                return Ok(());
            }
            let token_digest =
                codex_app_login::owned_refresh_token_digest(current.document.as_slice())?;
            match &journal.pending {
                Some(pending)
                    if pending.phase == Phase::Prepared
                        && pending.base_generation == current.revision.generation
                        && pending.base_digest == current.revision.digest
                        && pending.token_digest.as_deref() == Some(token_digest.as_str()) => {}
                Some(_) => return Err("native quota refresh is uncertain".into()),
                None => {
                    journal.pending = Some(Pending {
                        operation: actor,
                        phase: Phase::Prepared,
                        quarantine_reason: None,
                        base_generation: current.revision.generation,
                        base_digest: current.revision.digest,
                        token_digest: Some(token_digest),
                        response: None,
                    })
                }
            }
            journal.transition = JournalTransition {
                expected: Some(witness),
                kind: TransitionKind::Attempt,
            };
            claimed = transaction::persist_owned(claimed, &mut retained)?;
            let transition = claimed.transition()?;
            let next = transition.next().clone();
            grant.anchor.compare_exchange(&transition)?;
            result = Some(NativePreparation::Exchange(
                Box::new(transaction::PreparedExchange {
                    claimed,
                    witness: next,
                    document: current.document,
                }),
                Box::new(policy),
            ));
            Ok(())
        },
    );
    if let Some(retained) = retained {
        return Err(OwnerRefreshError::Retaining(retained));
    }
    admitted.map_err(|_| OwnerRefreshError::Unavailable)?;
    result.ok_or(OwnerRefreshError::Unavailable)
}

#[cfg(test)]
#[path = "codex_grant_native_usage_tests.rs"]
mod tests;
