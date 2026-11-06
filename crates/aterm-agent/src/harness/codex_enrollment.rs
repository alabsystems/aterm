// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Enrollment operation custody inside the account store's durable publication.
//! This module grants no provider, process, credential or session assignment authority.

use super::accounts::{self, Binding};
use aterm_json::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    status: Status,
}

impl Record {
    fn intent(&self) -> Option<RejectedIntent> {
        match (&self.request, &self.rejected) {
            (Some(request), None) if request.valid() => Some(request.intent()),
            (None, Some(intent))
                if intent.valid()
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
    #[serde(skip)] std::collections::BTreeSet<String>,
    #[serde(skip)] std::collections::BTreeSet<String>,
);

impl Operations {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn has_rejected(&self) -> bool {
        self.0.values().any(|record| record.rejected.is_some())
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
                    || match record.status.phase {
                        Phase::Applied => !record.status.generation.is_some_and(|n| n > 0),
                        _ => record.status.generation.is_some(),
                    }
            })
        {
            return Err("invalid enrollment custody".into());
        }
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
                status: status.clone(),
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
                status: Status {
                    phase: Phase::Running,
                    generation: None,
                },
            },
        );
        Ok(true)
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
    pub(crate) fn publishing(&mut self, request: &Request) -> Result<(), String> {
        self.require_running(request)?;
        self.0
            .get_mut(&request.id)
            .expect("checked record")
            .status
            .phase = Phase::Applied;
        Ok(())
    }

    pub(crate) fn retire(&mut self, proof: &Retired) -> Result<(), String> {
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
        self.1.insert(proof.request.id.clone());
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
                        || (old.status.phase != Phase::Running
                            && new != old
                            && !(old.status.phase == Phase::CancelRequested
                                && new.status.phase == Phase::Cancelled
                                && new.status.generation.is_none()
                                && self.1.contains(id)))
                })
            })
        {
            return Err("enrollment operation custody was erased or altered".into());
        }
        for id in &self.1 {
            if !before
                .0
                .get(id)
                .is_some_and(|old| old.status.phase == Phase::CancelRequested)
                || !self
                    .0
                    .get(id)
                    .is_some_and(|new| new.status.phase == Phase::Cancelled)
            {
                return Err("invalid enrollment retirement evidence".into());
            }
        }
        for (id, record) in &mut self.0 {
            if record.intent().is_none_or(|intent| id != &intent.id) {
                return Err("invalid enrollment operation".into());
            }
            let old = before.0.get(id);
            if record.status.phase == Phase::Cancelled
                && !old.is_some_and(|old| old.status.phase == Phase::Cancelled)
                && !self.1.contains(id)
            {
                return Err("enrollment cancellation has no retirement evidence".into());
            }
            if old.is_none() {
                let new_row = record.request.is_some()
                    && record.rejected.is_none()
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
        self.1.clear();
        self.2.clear();
        Ok(())
    }
}

/// Work owner minted before dispatch; only a successful durable new admission
/// enables its request-bound retirement proof.
pub struct Work {
    request: Request,
    scope: super::account_work::Scope,
    admitted: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Native adapter handle which never retains provider work.
pub struct Control {
    request: Request,
    cancellation: super::account_work::Cancellation,
    admitted: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Evidence of retirement for the exact durably admitted request. Fields are
/// private; a newly minted scope for a replayed ID cannot construct this proof.
pub struct Retired {
    request: Request,
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
    let control = Control {
        request: request.clone(),
        cancellation: scope.cancellation(),
        admitted: admitted.clone(),
    };
    (
        Work {
            request,
            scope,
            admitted,
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
        let _scope = self.scope;
        super::cli::enroll_codex_operation_admitted(env, host, &self.request, &|| {
            self.admitted
                .store(true, std::sync::atomic::Ordering::Release)
        })
    }

    pub fn run(self, env: &super::cli::Env) -> Result<Status, String> {
        super::cli::enroll_codex_operation_admitted(
            env,
            &super::cli::LiveAccounts::manual_work(self.scope),
            &self.request,
            &|| {
                self.admitted
                    .store(true, std::sync::atomic::Ordering::Release)
            },
        )
    }
}

impl Control {
    /// Revoke durable publication before requesting provider cancellation.
    pub fn cancel(&self, env: &super::cli::Env) -> Result<Status, String> {
        let status = cancel(env, &self.request)?;
        if status.phase == Phase::CancelRequested {
            self.cancellation.cancel();
        }
        Ok(status)
    }

    /// Work-lifetime observation for bounded runtime cleanup, not a publication
    /// or cancellation-completion grant. Only `retired` carries that authority.
    pub fn work_retired(&self) -> bool {
        self.cancellation.retired()
    }

    pub fn retired(&self) -> Option<Retired> {
        (self.admitted.load(std::sync::atomic::Ordering::Acquire) && self.cancellation.retired())
            .then(|| Retired {
                request: self.request.clone(),
            })
    }
}

/// Run one operation through the existing native staged sign-in. An untrusted
/// control request must first be admitted by the native session adapter.
pub fn enroll(
    env: &super::cli::Env,
    scope: super::account_work::Scope,
    request: &Request,
) -> Result<Status, String> {
    super::cli::enroll_codex_operation(env, &super::cli::LiveAccounts::manual_work(scope), request)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        Request {
            id: "a".repeat(32),
            caller: "b".repeat(64),
            label: "work".into(),
            revision: "c".repeat(64),
        }
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
        assert!(
            running
                .clone()
                .retire(&Retired {
                    request: request.clone()
                })
                .is_err()
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
        assert!(retired.retire(&Retired { request: wrong }).is_err());
        retired
            .retire(&Retired {
                request: request.clone(),
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
        assert!(restored.retire(&Retired { request: wrong }).is_err());
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
