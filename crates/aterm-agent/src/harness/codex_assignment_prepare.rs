// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Prepared targets are durable correlation, not a process freeze or a grant.
//! A usable proof exists only inside the claimed book and current owner callback.

use super::*;

#[path = "codex_assignment_prepare_cancel.rs"]
pub(in crate::harness) mod cancel;
#[path = "codex_assignment_target_install.rs"]
pub(in crate::harness) mod install;

pub(in crate::harness) const MAX_PREPARED: usize = 64;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    operation: String,
    pool_revision: String,
    expected_assignment: u64,
    account: Option<String>,
}

impl Body {
    fn from_request(request: &protocol::Request) -> Result<Self, String> {
        let protocol::Command::Prepare {
            pool_revision,
            expected_assignment,
            selection,
        } = &request.command
        else {
            return Err("assignment request is not preparation".into());
        };
        let body = Self {
            operation: request.operation.clone(),
            pool_revision: pool_revision.clone(),
            expected_assignment: *expected_assignment,
            account: match selection {
                protocol::Selection::Auto => None,
                protocol::Selection::Account(id) => Some(id.clone()),
            },
        };
        if !body.valid() {
            return Err("assignment preparation is invalid".into());
        }
        Ok(body)
    }
    fn valid(&self) -> bool {
        hex(&self.operation, 32)
            && hex(&self.pool_revision, 64)
            && self
                .account
                .as_deref()
                .is_none_or(|account| hex(account, 64))
    }
    fn selection(&self) -> protocol::Selection {
        self.account
            .clone()
            .map_or(protocol::Selection::Auto, protocol::Selection::Account)
    }
}

/// Native endpoint provenance from an authenticated producer document. No JWT
/// claim or legacy refresh default can construct this evidence.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OAuth {
    issuer: String,
    pub(super) token_endpoint: String,
    pub(super) client: String,
}

impl OAuth {
    pub(super) fn from_document(document: &[u8]) -> Result<Self, String> {
        let value: aterm_json::Value =
            aterm_json::from_slice(document).map_err(|_| "assignment target document invalid")?;
        let provenance = value
            .get("tokens")
            .and_then(|v| v.get("oauth_provenance"))
            .ok_or("assignment target has no native OAuth provenance")?;
        let issuer = provenance
            .get("issuer")
            .and_then(aterm_json::Value::as_str)
            .ok_or("assignment target issuer unavailable")?;
        let client = provenance
            .get("client_id")
            .and_then(aterm_json::Value::as_str)
            .ok_or("assignment target client unavailable")?;
        let result = Self {
            issuer: issuer.into(),
            token_endpoint: format!("{}/oauth/token", issuer.trim_end_matches('/')),
            client: client.into(),
        };
        if !result.valid() {
            return Err("assignment target OAuth provenance invalid".into());
        }
        Ok(result)
    }
    fn valid(&self) -> bool {
        if self.issuer.len() > 2048
            || self.client.is_empty()
            || self.client.len() > 256
            || self.client.trim() != self.client
            || self.client.chars().any(char::is_control)
            || self
                .issuer
                .bytes()
                .any(|b| b <= 0x20 || b >= 0x7f || matches!(b, b'\\' | b'?' | b'#'))
            || self.token_endpoint != format!("{}/oauth/token", self.issuer.trim_end_matches('/'))
        {
            return false;
        }
        let Some(issuer) = aterm_http::Uri::parse(&self.issuer) else {
            return false;
        };
        let Some(token) = aterm_http::Uri::parse(&self.token_endpoint) else {
            return false;
        };
        issuer.scheme() == aterm_http::Scheme::Https
            && token.scheme() == issuer.scheme()
            && token.host() == issuer.host()
            && token.port() == issuer.port()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    binding: owner::GrantBinding,
    identity: super::super::codex_renewal::Identity,
    oauth: OAuth,
    credential_floor: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    body: Body,
    source: owner::GrantBinding,
    source_authority: SourceAuthority,
    // None is a durable accepted intent, never a prepared-target proof.
    target: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) commit_boundary: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    cancelled: bool,
}

impl Record {
    pub(in crate::harness::codex_assignment) fn matches_ack(
        &self,
        ack: &super::target_ack::Observation,
    ) -> bool {
        self.body.operation == ack.operation
            && self.commit_boundary.as_deref() == Some(ack.boundary.as_str())
            && self.body.expected_assignment.checked_add(1) == Some(ack.assignment_generation)
            && self.target.as_ref().is_some_and(|target| {
                target.identity.account == ack.account
                    && target.identity.user == ack.user
                    && target.credential_floor <= ack.credential_generation
            })
    }
    fn valid(&self) -> bool {
        self.body.valid()
            && (!self.cancelled || self.commit_boundary.is_none())
            && self
                .commit_boundary
                .as_deref()
                .is_none_or(|boundary| hex(boundary, 32))
            && (self.commit_boundary.is_none() || self.target.is_some())
            && self.source_authority.valid(self.body.expected_assignment)
            && self.source.generation > 0
            && self.source.request.valid()
            && self.source.reference.valid()
            && self.target.as_ref().is_none_or(|target| {
                target.binding.generation > 0
                    && target.binding.request.valid()
                    && target.binding.reference.valid()
                    && hex(&target.identity.account, 64)
                    && hex(&target.identity.user, 64)
                    && target.credential_floor > 0
                    && target.oauth.valid()
                    && self.body.account.as_ref().map_or_else(
                        || self.source.request.label != target.binding.request.label,
                        |id| {
                            *id == super::super::codex_enrollment::account_id(
                                &target.binding.request.label,
                            )
                        },
                    )
            })
    }
}

pub(super) fn current_record_valid(book: &Book) -> bool {
    let SourceAuthority::Assignment { operation } = &book.current.authority else {
        return true;
    };
    book.prepared
        .iter()
        .find(|record| record.body.operation == *operation)
        .is_none_or(|record| {
            !record.cancelled
                && record.commit_boundary == book.current.boundary
                && record.body.expected_assignment == book.current.previous_generation
                && record
                    .target
                    .as_ref()
                    .is_some_and(|target| target.binding == book.current.binding)
        })
}

pub(super) fn pending_for_current(book: &Book) -> bool {
    book.prepared.iter().any(|record| {
        record.commit_boundary.is_some()
            && record.body.expected_assignment == book.current.generation
            && record.source == book.current.binding
            && record.source_authority == book.current.authority
    })
}

fn same_prepared(a: &Record, b: &Record) -> bool {
    a.body == b.body
        && a.source == b.source
        && a.source_authority == b.source_authority
        && a.target == b.target
        && a.cancelled == b.cancelled
}

pub(super) fn records_valid(records: &[Record]) -> bool {
    records.len() <= MAX_PREPARED
        && records.iter().enumerate().all(|(index, record)| {
            record.valid()
                && records[..index]
                    .iter()
                    .all(|old| old.body.operation != record.body.operation)
        })
}

fn admitted<'a>(records: &'a [Record], body: &Body) -> Result<Option<&'a Record>, String> {
    let prior = records
        .iter()
        .find(|record| record.body.operation == body.operation);
    if prior.is_some_and(|record| record.body != *body) {
        return Err("assignment operation body changed".into());
    }
    if prior.is_some_and(|record| record.cancelled) {
        return Err("assignment operation cancelled".into());
    }
    if prior.is_none() && records.len() >= MAX_PREPARED {
        return Err("assignment preparation capacity unavailable".into());
    }
    Ok(prior)
}

/// Borrowed from a currently claimed native book and authenticated target.
/// This grants no freeze, provider dispatch, materialization or assignment.
pub(in crate::harness) struct PreparedAssignment<'a, 'grant> {
    record: &'a Record,
    source: &'a super::super::codex_source_registry::ReadySourceProof,
    target: &'a owner::AdmittedGrant<'grant>,
}

/// Owned continuation minted only inside the claimed prepared-target callback.
/// It is metadata authority to re-admit this target, never credential bytes or
/// a process-freeze proof. It cannot be cloned or deserialized.
pub(in crate::harness) struct PreparedTarget {
    record: Record,
    caller: Caller,
    preview: Option<super::policy::PreparedPreview>,
}

impl PreparedTarget {
    pub(in crate::harness) fn policy_preview(&self) -> Option<&super::policy::PreparedPreview> {
        self.preview.as_ref()
    }
    pub(in crate::harness) fn operation(&self) -> &str {
        &self.record.body.operation
    }
    pub(in crate::harness) fn caller(&self) -> &Caller {
        &self.caller
    }
    pub(in crate::harness) fn source(&self) -> &owner::GrantBinding {
        &self.record.source
    }
    pub(in crate::harness) fn intent(&self) -> TargetIntent {
        TargetIntent {
            record: self.record.clone(),
            caller: self.caller.clone(),
        }
    }
    pub(in crate::harness) fn matches_intent(&self, intent: &TargetIntent) -> bool {
        self.caller == intent.caller && same_prepared(&self.record, &intent.record)
    }
    pub(in crate::harness) fn matches(&self, other: &Self) -> bool {
        self.caller == other.caller
            && same_prepared(&self.record, &other.record)
            && self.preview == other.preview
    }
}

/// Owned metadata snapshot for dropping registry guards before bounded I/O.
/// It is not an admission, installation, source, or factory authority.
pub(in crate::harness) struct TargetIntent {
    record: Record,
    caller: Caller,
}
impl TargetIntent {
    pub(in crate::harness) fn operation(&self) -> &str {
        &self.record.body.operation
    }
    pub(in crate::harness) fn assignment_generation(&self) -> Option<u64> {
        self.record.body.expected_assignment.checked_add(1)
    }
    pub(in crate::harness) fn caller(&self) -> &Caller {
        &self.caller
    }
    pub(in crate::harness) fn target_binding(&self) -> Option<&owner::GrantBinding> {
        self.record.target.as_ref().map(|target| &target.binding)
    }
}

pub(super) fn origin_namespace_matches(book: &Book, intent: &TargetIntent) -> Result<bool, String> {
    let Some(record) = book
        .prepared
        .iter()
        .find(|record| record.body.operation == intent.operation())
    else {
        return Ok(false);
    };
    // This is the already-admitted source A's namespace, not permission to
    // prepare/install target B. A cancelled proposal must not strand A's
    // actual work while its signed cancellation/retirement settles.
    let mut expected = intent.record.clone();
    expected.cancelled = record.cancelled;
    Ok(same_prepared(record, &expected)
        && record.commit_boundary.is_none()
        && book.current.binding == record.source
        && book.current.authority == record.source_authority
        && book.current.generation == record.body.expected_assignment
        && matches!(book.current.phase, Phase::Startup | Phase::Applied))
}

impl std::fmt::Debug for PreparedTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedTarget(redacted)")
    }
}

#[cfg(all(test, unix))]
pub(in crate::harness) fn reservation_metadata_fixture(
    caller: Caller,
    source: owner::GrantBinding,
    target: owner::GrantBinding,
) -> PreparedTarget {
    // Explicit synthetic metadata injection for registry lifecycle tests only.
    // It does not pass the prepared callback, authenticate an owner or qualify
    // a live source/process and must never be counted as an admission test.
    PreparedTarget {
        caller,
        preview: None,
        record: Record {
            body: Body {
                operation: "1".repeat(32),
                pool_revision: "2".repeat(64),
                expected_assignment: 0,
                account: (source.request.label == target.request.label)
                    .then(|| crate::harness::codex_enrollment::account_id(&target.request.label)),
            },
            source,
            source_authority: SourceAuthority::Startup {
                nonce: "3".repeat(32),
            },
            commit_boundary: None,
            cancelled: false,
            target: Some(Target {
                binding: target,
                identity: super::super::codex_renewal::Identity::from_native("fixture", "fixture"),
                oauth: OAuth {
                    issuer: "https://auth.example.test".into(),
                    token_endpoint: "https://auth.example.test/oauth/token".into(),
                    client: "fixture".into(),
                },
                credential_floor: 1,
            }),
        },
    }
}

impl<'grant> PreparedAssignment<'_, 'grant> {
    pub(in crate::harness) fn retain_target(&self) -> PreparedTarget {
        PreparedTarget {
            record: self.record.clone(),
            caller: self.source.caller().clone(),
            preview: self.policy_preview().ok(),
        }
    }
    fn policy_preview(&self) -> Result<super::policy::PreparedPreview, String> {
        let (_, _, source, _) = self.source.gate_source();
        let source_policy = source
            .provider_policy()
            .ok_or("source policy unavailable")?;
        let target_policy = super::policy::ProviderPolicy::capture(self.target)?;
        if source_policy.binding() != target_policy.binding()
            || source_policy.fedramp() != target_policy.fedramp()
        {
            return Err("assignment provider policy differs".into());
        }
        let binding_hash = super::super::codex_source_registry::immutable_binding_hash;
        Ok(super::policy::PreparedPreview {
            operation: self.record.body.operation.clone(),
            pool_revision: self.record.body.pool_revision.clone(),
            expected_assignment: self.record.body.expected_assignment,
            target_ref: super::super::codex_enrollment::account_id(
                &self.target.binding().request.label,
            ),
            auth_mode: "chatgpt",
            account: self.target.identity().account.clone(),
            user: self.target.identity().user.clone(),
            source_grant: binding_hash(source.binding())
                .map_err(|_| "source policy binding unavailable")?,
            target_grant: binding_hash(self.target.binding())
                .map_err(|_| "target policy binding unavailable")?,
            source_provider: source_policy.binding().into(),
            target_provider: target_policy.binding().into(),
            source_fedramp: source_policy.fedramp(),
            target_fedramp: target_policy.fedramp(),
            target_refresh_endpoint: target_policy.endpoint().into(),
        })
    }
    pub(super) fn operation(&self) -> &str {
        &self.record.body.operation
    }
    pub(super) fn target(&self) -> &owner::AdmittedGrant<'grant> {
        self.target
    }
    pub(super) fn source(&self) -> &super::super::codex_source_registry::ReadySourceProof {
        self.source
    }
}

impl std::fmt::Debug for PreparedAssignment<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedAssignment(redacted)")
    }
}

fn current(
    env: &Env,
    book: &Book,
    source: &super::super::codex_source_registry::ReadySourceProof,
    body: &Body,
) -> bool {
    let (_, _, handle, _) = source.gate_source();
    env.home
        .as_deref()
        .is_some_and(|home| source.gate_source().3.matches_home(home))
        && source.current()
        && book.valid(source.caller())
        && book.current.generation == body.expected_assignment
        && book.current.binding == *handle.binding()
        && handle.assignment_generation() == body.expected_assignment
        && book.current.authority == *handle.source_authority()
        && matches!(book.current.phase, Phase::Startup | Phase::Applied)
}

fn publish(claimed: &mut Claimed, identity: &age::Identity) -> Result<(), String> {
    if claimed.pending_install.is_some() {
        return Err("assignment installation custody busy".into());
    }
    if !claimed.book.valid(&claimed.book.caller) {
        return Err("assignment preparation book is invalid".into());
    }
    let mac = signature(identity, &claimed.book)?;
    // Serialize by reference: a failed save leaves the exact admitted intent in
    // memory, and every replay retries this same write before minting a proof.
    #[derive(Serialize)]
    struct BorrowedEnvelope<'a> {
        book: &'a Book,
        mac: String,
    }
    let bytes = SecretBytes::new(
        aterm_json::to_vec(&BorrowedEnvelope {
            book: &claimed.book,
            mac,
        })
        .map_err(|_| "assignment preparation encoding unavailable")?,
    );
    claimed.store.save(bytes.as_slice())?;
    claimed.store.unchanged().map_err(str::to_owned)
}

/// Prepare a target for an already authenticated registered source. This does
/// not advertise transport support or grant permission to freeze that source.
/// # Errors
/// Stale source/pool policy, unavailable target custody, altered operation body,
/// unsupported provenance, capacity, or durable publication failure.
pub fn prepare(
    env: &Env,
    source: &super::super::codex_source_registry::ReadySourceProof,
    request: &protocol::Request,
) -> Result<protocol::Status, String> {
    with_prepared(env, source, request, |prepared| {
        if !prepared.source().current() {
            return Err("assignment source changed".into());
        }
        Ok(protocol::Status {
            operation: prepared.operation().to_owned(),
            phase: protocol::Phase::Prepared,
            boundary: None,
            assignment_generation: None,
            credential_generation: None,
            target_ref: Some(super::super::codex_enrollment::account_id(
                &prepared.target().binding().request.label,
            )),
            selection_mode: Some(if prepared.record.body.account.is_some() {
                protocol::SelectionMode::Manual
            } else {
                protocol::SelectionMode::Auto
            }),
            reason: None,
        })
    })
}

pub(in crate::harness) fn with_prepared<R>(
    env: &Env,
    source: &super::super::codex_source_registry::ReadySourceProof,
    request: &protocol::Request,
    use_prepared: impl FnOnce(PreparedAssignment<'_, '_>) -> Result<R, String>,
) -> Result<R, String> {
    platform::require_available()?;
    let body = Body::from_request(request)?;
    let home = env.home.as_deref().ok_or("assignment home unavailable")?;
    if !source.gate_source().3.matches_home(home) {
        return Err("assignment source namespace changed".into());
    }
    let lease = open(env, source.caller())?;
    let identity = platform::machine_identity(home)
        .map_err(|_| "assignment identity unavailable")?
        .ok_or("assignment identity unavailable")?;
    let prior = {
        let mut claimed = lease.try_lock().map_err(|_| "assignment custody busy")?;
        if claimed.pending_install.is_some() {
            return Err("assignment installation custody busy".into());
        }
        if !current(env, &claimed.book, source, &body) {
            return Err("assignment source changed".into());
        }
        claimed.store.unchanged().map_err(str::to_owned)?;
        if claimed.book.current.authority.native_intent() == Some(body.operation.as_str())
            && !claimed
                .book
                .prepared
                .iter()
                .any(|record| record.body.operation == body.operation)
        {
            return Err("assignment operation already current".into());
        }
        let prior = admitted(&claimed.book.prepared, &body)?.cloned();
        let retained = match prior {
            Some(prior) => prior,
            None => {
                let intent = Record {
                    body: body.clone(),
                    source: claimed.book.current.binding.clone(),
                    source_authority: claimed.book.current.authority.clone(),
                    commit_boundary: None,
                    cancelled: false,
                    target: None,
                };
                claimed.book.prepared.push(intent.clone());
                claimed.book.schema = if !claimed.book.acknowledged.is_empty() {
                    5
                } else if claimed
                    .book
                    .prepared
                    .iter()
                    .any(|record| record.commit_boundary.is_some())
                {
                    4
                } else {
                    3
                };
                intent
            }
        };
        if retained.source != claimed.book.current.binding
            || retained.source_authority != claimed.book.current.authority
        {
            return Err("assignment intent source changed".into());
        }
        // Admission is durable before credential contention. An unknown/lost
        // reply therefore cannot turn this ID into a different request later.
        publish(&mut claimed, &identity)?;
        retained
    };
    let selection = body.selection();
    let source_binding = prior.source.clone();
    owner::with_selected_target(
        env,
        source.caller(),
        &body.pool_revision,
        &selection,
        &source_binding,
        prior.target.as_ref().map(|target| &target.binding),
        |grant| {
            // Account/owner custody precedes the book, matching native source
            // use. The initial book observation never remains locked while
            // acquiring credential custody.
            let mut claimed = lease.try_lock().map_err(|_| "assignment custody busy")?;
            if claimed.pending_install.is_some() {
                return Err("assignment installation custody busy".into());
            }
            if !current(env, &claimed.book, source, &body) {
                return Err("assignment source changed".into());
            }
            claimed.store.unchanged().map_err(str::to_owned)?;
            let current_prior = admitted(&claimed.book.prepared, &body)?
                .ok_or("assignment intent disappeared")?
                .clone();
            if current_prior.source != prior.source
                || current_prior.source_authority != prior.source_authority
                || prior
                    .target
                    .as_ref()
                    .is_some_and(|old| current_prior.target.as_ref() != Some(old))
            {
                return Err("assignment preparation changed".into());
            }
            let target = Target {
                binding: grant.binding().clone(),
                identity: grant.identity().clone(),
                oauth: OAuth::from_document(grant.document())?,
                credential_floor: grant.revision().generation,
            };
            if let Some(old) = &current_prior.target {
                if old.binding != target.binding
                    || old.identity != target.identity
                    || old.oauth != target.oauth
                    || target.credential_floor < old.credential_floor
                {
                    return Err("prepared assignment target changed".into());
                }
            } else {
                let retained = claimed
                    .book
                    .prepared
                    .iter_mut()
                    .find(|record| record.body.operation == body.operation)
                    .ok_or("assignment intent disappeared")?;
                retained.target = Some(target);
            }
            publish(&mut claimed, &identity)?;
            let retained = claimed
                .book
                .prepared
                .iter()
                .find(|record| record.body.operation == body.operation)
                .ok_or("prepared assignment disappeared")?;
            let result = use_prepared(PreparedAssignment {
                record: retained,
                source,
                target: &grant,
            })?;
            if !current(env, &claimed.book, source, &body) {
                return Err("assignment source changed".into());
            }
            claimed.store.unchanged().map_err(str::to_owned)?;
            Ok(result)
        },
    )
}

#[cfg(test)]
#[path = "codex_assignment_prepare_tests.rs"]
mod tests;
