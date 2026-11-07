// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Mutable encrypted revisions below an immutable, producer-authenticated grant.
//! A journal is not publication or assignment authority. Its caller must hold
//! the parent's current canonical grant admission before using a revision.

use super::*;
use crate::harness::codex_assignment::SourceAuthority;
use crate::harness::codex_enrollment::{OwnerAttempt, OwnerWitness};
use crate::harness::{codex_session_slot as slot, pending_native_store};

const MAX_JOURNAL: usize = 4 * 1024 * 1024;
const MAX_OPERATIONS: usize = 256;

#[path = "codex_grant_refresh_transaction.rs"]
mod transaction;
pub(in crate::harness) use transaction::with_refreshed_owner;

pub(super) struct CredentialRevision {
    pub(super) identity: Identity,
    pub(super) revision: slot::Revision,
    pub(super) document: SecretBytes,
    canonical_alternate: Option<SecretBytes>,
}
impl CredentialRevision {
    pub(super) fn canonical_matches(&self, bytes: &[u8]) -> bool {
        aterm_digest::ct_eq(self.document.as_slice(), bytes)
            || self
                .canonical_alternate
                .as_ref()
                .is_some_and(|other| aterm_digest::ct_eq(other.as_slice(), bytes))
    }
}
impl std::fmt::Debug for CredentialRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialRevision(redacted)")
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Current {
    identity: Identity,
    generation: u64,
    digest: String,
    document: String,
}
impl Drop for Current {
    fn drop(&mut self) {
        self.document.zeroize();
    }
}
impl Current {
    fn open(&self) -> Result<CredentialRevision, String> {
        if self.generation == 0
            || !valid_hex(&self.digest, 64)
            || !valid_hex(&self.identity.account, 64)
            || !valid_hex(&self.identity.user, 64)
            || self.document.len() > MAX_DOCUMENT.div_ceil(3) * 4
        {
            return Err("owner revision invalid".into());
        }
        let document = SecretBytes::new(
            aterm_codec::base64::decode_strict(self.document.as_bytes())
                .map_err(|_| "owner revision invalid")?,
        );
        if document.is_empty()
            || document.len() > MAX_DOCUMENT
            || self.digest
                != aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
                    document.as_slice(),
                ))
        {
            return Err("owner revision invalid".into());
        }
        Ok(CredentialRevision {
            identity: self.identity.clone(),
            revision: slot::Revision {
                generation: self.generation,
                digest: self.digest.clone(),
            },
            document,
            canonical_alternate: None,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    id: String,
    caller: slot::Caller,
    slot: String,
    expected_digest: String,
    assignment_generation: u64,
    source_authority: SourceAuthority,
    credential_generation: u64,
}
impl Operation {
    fn valid(&self) -> bool {
        valid_hex(&self.id, 32)
            && valid_hex(&self.expected_digest, 64)
            && self.source_authority.valid(self.assignment_generation)
            && self.credential_generation > 0
            && !self.slot.is_empty()
            && self.slot.len() <= 256
            && self.slot.len().is_multiple_of(2)
            && valid_hex(&self.slot, self.slot.len())
            && self.caller.pid > 1
            && self.caller.birth > 0
            && self.caller.sid.starts_with("s-")
            && self.caller.sid.len() <= 64
            && !self.caller.sid.chars().any(char::is_control)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    Attempted,
    Responded,
    Publishing,
    Rejected,
    Quarantined,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    status: u16,
    received_at: Option<u64>,
    body: String,
}
impl Drop for Response {
    fn drop(&mut self) {
        self.body.zeroize();
    }
}
impl Response {
    fn bytes(&self) -> Result<SecretBytes, String> {
        if self.body.len() > codex_app_login::MESSAGE_MAX.div_ceil(3) * 4 {
            return Err("retained refresh response invalid".into());
        }
        let bytes = SecretBytes::new(
            aterm_codec::base64::decode_strict(self.body.as_bytes())
                .map_err(|_| "retained refresh response invalid")?,
        );
        if bytes.len() > codex_app_login::MESSAGE_MAX {
            return Err("retained refresh response invalid".into());
        }
        Ok(bytes)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    operation: Operation,
    phase: Phase,
    base_generation: u64,
    base_digest: String,
    token_digest: Option<String>,
    response: Option<Response>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completed {
    operation: Operation,
    // Actual provider attempts retain lineage evidence; coalesced deliveries do not.
    spent_token_digest: Option<String>,
    generation: u64,
    digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    binding: GrantBinding,
    current: Current,
    previous_current: Option<Current>,
    transition: JournalTransition,
    pending: Option<Pending>,
    completed: Vec<Completed>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TransitionKind {
    Fresh,
    Attempt,
    Response,
    Promote,
    NotSent,
    Metadata,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalTransition {
    expected: Option<OwnerWitness>,
    kind: TransitionKind,
}

fn matches_revision(current: &Current, revision: &slot::Revision) -> bool {
    current.generation == revision.generation && current.digest == revision.digest
}

fn pending_matches(pending: &Pending, attempt: &OwnerAttempt) -> bool {
    let Ok(digest) = operation_digest(&pending.operation) else {
        return false;
    };
    pending.operation.id == attempt.operation
        && digest == attempt.request_digest
        && pending.base_generation == attempt.base_generation
        && pending.base_digest == attempt.base_digest
        && pending.token_digest.as_deref() == Some(attempt.token_digest.as_str())
}

fn operation_digest(operation: &Operation) -> Result<String, String> {
    let bytes = aterm_json::to_vec(&("aterm/codex-owner-refresh-operation/v1", operation))
        .map_err(|_| "owner operation unavailable")?;
    Ok(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(&bytes),
    ))
}

fn journal_pending(journal: &Journal) -> Result<Option<OwnerAttempt>, String> {
    let Some(pending) = journal.pending.as_ref() else {
        return Ok(None);
    };
    if pending.phase == Phase::Publishing {
        return Ok(None);
    }
    Ok(Some(OwnerAttempt {
        operation: pending.operation.id.clone(),
        request_digest: operation_digest(&pending.operation)?,
        base_generation: pending.base_generation,
        base_digest: pending.base_digest.clone(),
        token_digest: pending
            .token_digest
            .clone()
            .ok_or("owner token evidence absent")?,
    }))
}

impl JournalTransition {
    fn permits(&self, journal: &Journal) -> bool {
        let Some(expected) = self.expected.as_ref() else {
            return self.kind == TransitionKind::Fresh
                && journal.current.generation == 1
                && journal.previous_current.is_none()
                && journal.pending.is_none()
                && journal.completed.is_empty();
        };
        if !expected.valid() {
            return false;
        }
        let same = matches_revision(&journal.current, &expected.revision);
        match self.kind {
            TransitionKind::Fresh => false,
            TransitionKind::Attempt => {
                same && journal.previous_current.is_none()
                    && expected.pending.as_ref().is_none_or(|attempt| {
                        journal
                            .pending
                            .as_ref()
                            .is_some_and(|pending| pending_matches(pending, attempt))
                    })
                    && journal.pending.as_ref().is_some_and(|pending| {
                        matches!(pending.phase, Phase::Prepared | Phase::Attempted)
                            && pending.base_generation == journal.current.generation
                            && pending.base_digest == journal.current.digest
                    })
            }
            TransitionKind::Response => {
                same && journal.previous_current.is_none()
                    && expected.pending.as_ref().is_some_and(|attempt| {
                        journal.pending.as_ref().is_some_and(|pending| {
                            pending_matches(pending, attempt)
                                && matches!(
                                    pending.phase,
                                    Phase::Responded | Phase::Rejected | Phase::Quarantined
                                )
                        })
                    })
            }
            TransitionKind::Promote => expected.pending.as_ref().is_some_and(|attempt| {
                journal.previous_current.as_ref().is_some_and(|previous| {
                    previous.open().is_ok()
                        && matches_revision(previous, &expected.revision)
                        && previous.identity == journal.current.identity
                        && previous.generation.checked_add(1) == Some(journal.current.generation)
                }) && journal.pending.as_ref().is_some_and(|pending| {
                    pending_matches(pending, attempt)
                        && pending.phase == Phase::Publishing
                        && pending.response.is_some()
                })
            }),
            TransitionKind::NotSent => {
                same && journal.previous_current.is_none()
                    && expected.pending.as_ref().is_some_and(|attempt| {
                        journal.pending.as_ref().is_some_and(|pending| {
                            pending_matches(pending, attempt)
                                && pending.phase == Phase::Prepared
                                && pending.response.is_none()
                        })
                    })
            }
            TransitionKind::Metadata => {
                same && expected.pending.is_none()
                    && journal.previous_current.is_none()
                    && journal.pending.is_none()
            }
        }
    }
}
impl Journal {
    fn valid(&self) -> bool {
        if self.schema != 3
            || !self.binding.request.valid()
            || !self.binding.reference.valid()
            || self.binding.generation == 0
            || self.current.open().is_err()
            || self.completed.len() > MAX_OPERATIONS
            || !self.transition.permits(self)
        {
            return false;
        }
        let mut ids = std::collections::BTreeSet::new();
        if self.completed.iter().any(|done| {
            !done.operation.valid()
                || done
                    .spent_token_digest
                    .as_ref()
                    .is_some_and(|value| !valid_hex(value, 64))
                || done.generation == 0
                || !valid_hex(&done.digest, 64)
                || done.generation > self.current.generation
                || (done.generation == self.current.generation
                    && done.digest != self.current.digest)
                || !ids.insert(done.operation.id.as_str())
        }) {
            return false;
        }
        self.pending.as_ref().is_none_or(|pending| {
            if !pending.operation.valid()
                || ids.contains(pending.operation.id.as_str())
                || pending.base_generation == 0
                || !valid_hex(&pending.base_digest, 64)
                || pending
                    .token_digest
                    .as_ref()
                    .is_some_and(|value| !valid_hex(value, 64))
            {
                return false;
            }
            match pending.phase {
                Phase::Prepared | Phase::Attempted => {
                    pending.token_digest.is_some() && pending.response.is_none()
                }
                Phase::Responded | Phase::Rejected | Phase::Quarantined => {
                    pending.token_digest.is_some()
                        && pending
                            .response
                            .as_ref()
                            .is_some_and(|response| response.bytes().is_ok())
                }
                Phase::Publishing => pending
                    .response
                    .as_ref()
                    .is_none_or(|response| response.bytes().is_ok()),
            }
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    journal: Journal,
    mac: String,
}

fn signature(identity: &age::Identity, journal: &Journal) -> Result<String, String> {
    let plaintext = SecretBytes::new(
        aterm_json::to_vec(&("aterm/codex-owner-revisions/v1", journal))
            .map_err(|_| "owner revision unavailable")?,
    );
    let salt = ring::hkdf::Salt::new(
        ring::hkdf::HKDF_SHA256,
        b"aterm/codex-owner-revision-key/v1\0",
    );
    let derived = salt.extract(identity.secret_bytes());
    let info: &[&[u8]] = &[b"refresh-journal"];
    let expanded = derived
        .expand(info, ring::hkdf::HKDF_SHA256)
        .map_err(|_| "owner revision unavailable")?;
    let mut raw = [0u8; 32];
    expanded
        .fill(&mut raw)
        .map_err(|_| "owner revision unavailable")?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &raw);
    raw.zeroize();
    Ok(aterm_codec::hex::encode_lower(
        ring::hmac::sign(&key, plaintext.as_slice()).as_ref(),
    ))
}

struct Claimed {
    store: pending_native_store::Store,
    identity: age::Identity,
    directory: PathBuf,
    journal: Option<Journal>,
}
impl Claimed {
    fn open(env: &Env, binding: &GrantBinding, create: bool) -> Result<Option<Self>, String> {
        Self::open_with_fresh(env, binding, create, None)
    }

    fn open_with_fresh(
        env: &Env,
        binding: &GrantBinding,
        create: bool,
        fresh: Option<&FreshActivationGuard<'_>>,
    ) -> Result<Option<Self>, String> {
        let (owner, home) = Store::location(env, &binding.request.id)?;
        let directory = owner.join("refresh");
        let identity = client::read_machine_identity(&client::Paths::locate_for_home(&home))
            .map_err(|_| "owner revision identity unavailable")?
            .ok_or("owner revision identity unavailable")?;
        let opened = if create {
            Some(pending_native_store::Store::open_owned(
                &directory,
                &home,
                MAX_JOURNAL,
            )?)
        } else {
            pending_native_store::Store::open_owned_existing(&directory, &home, MAX_JOURNAL)?
        };
        let Some((store, bytes)) = opened else {
            return Ok(None);
        };
        if store.recipient() != identity.recipient() {
            return Err("owner revision identity changed".into());
        }
        let journal = bytes
            .map(|bytes| -> Result<Journal, String> {
                let mut envelope: Envelope = aterm_json::from_slice(bytes.as_slice())
                    .map_err(|_| "owner revision record invalid")?;
                if !envelope.journal.valid()
                    || !valid_hex(&envelope.mac, 64)
                    || !aterm_digest::ct_eq(
                        envelope.mac.as_bytes(),
                        signature(&identity, &envelope.journal)?.as_bytes(),
                    )
                {
                    return Err("owner revision record invalid".into());
                }
                if envelope.journal.binding != *binding {
                    let guard = fresh.ok_or("owner revision binding changed")?;
                    let old = &envelope.journal;
                    if guard.binding() != binding
                        || old.binding.request != binding.request
                        || old.binding.reference != binding.reference
                        || old.binding.generation >= binding.generation
                        || old.transition.kind != TransitionKind::Fresh
                        || old.transition.expected.is_some()
                        || old.current.generation != 1
                        || old.previous_current.is_some()
                        || old.pending.is_some()
                        || !old.completed.is_empty()
                        || &old.current.identity != guard.identity()
                        || !aterm_digest::ct_eq(
                            old.current.open()?.document.as_slice(),
                            guard.document(),
                        )
                    {
                        return Err("owner revision binding changed".into());
                    }
                    // Only a verified pristine orphan may follow a new actual
                    // publication generation. The old signature was checked
                    // before changing its binding; the new payload is signed
                    // and durably settled by the initializer before use.
                    envelope.journal.binding = binding.clone();
                }
                Ok(envelope.journal)
            })
            .transpose()?;
        Ok(Some(Self {
            store,
            identity,
            directory,
            journal,
        }))
    }

    fn payload(&self) -> Result<SecretBytes, String> {
        #[derive(Serialize)]
        struct Signed<'a> {
            journal: &'a Journal,
            mac: String,
        }
        let journal = self
            .journal
            .as_ref()
            .ok_or("owner revision record absent")?;
        if !journal.valid() {
            return Err("owner revision record invalid".into());
        }
        let envelope = Signed {
            journal,
            mac: signature(&self.identity, journal)?,
        };
        Ok(SecretBytes::new(
            aterm_json::to_vec(&envelope).map_err(|_| "owner revision record unavailable")?,
        ))
    }

    fn view(&self, witness: &OwnerWitness) -> Result<CredentialRevision, String> {
        let journal = self
            .journal
            .as_ref()
            .ok_or("owner revision journal absent")?;
        let actual = self
            .store
            .sealed_digest()
            .ok_or("owner revision journal absent")?;
        let exact = witness.journal_digest == aterm_codec::hex::encode_lower(&actual);
        let descendant = journal.transition.expected.as_ref() == Some(witness);
        if !witness.valid()
            || (!exact && !descendant)
            || (exact && witness.pending != journal_pending(journal)?)
        {
            return Err("owner revision requires canonical reconciliation".into());
        }
        let mut visible = if matches_revision(&journal.current, &witness.revision) {
            journal.current.open()?
        } else if !exact && descendant {
            journal
                .previous_current
                .as_ref()
                .filter(|previous| matches_revision(previous, &witness.revision))
                .ok_or("owner revision requires canonical reconciliation")?
                .open()?
        } else {
            return Err("owner revision requires canonical reconciliation".into());
        };
        if visible.revision != journal.current.open()?.revision {
            visible.canonical_alternate = Some(journal.current.open()?.document);
        } else if let Some(previous) = journal.previous_current.as_ref() {
            visible.canonical_alternate = Some(previous.open()?.document);
        }
        Ok(visible)
    }

    fn transition(&mut self) -> Result<OwnerWitnessTransition, String> {
        let plaintext = self.payload()?;
        if !self
            .store
            .recover_exact_write(&self.identity, plaintext.as_slice())?
        {
            return Err("owner transition is not durably authenticated".into());
        }
        let journal = self
            .journal
            .as_ref()
            .ok_or("owner revision journal absent")?;
        let expected = journal
            .transition
            .expected
            .clone()
            .ok_or("fresh activation authority required")?;
        if journal.transition.kind == TransitionKind::Fresh {
            return Err("fresh activation authority required".into());
        }
        let pending = journal_pending(journal)?;
        let next = OwnerWitness {
            revision: slot::Revision {
                generation: journal.current.generation,
                digest: journal.current.digest.clone(),
            },
            journal_digest: aterm_codec::hex::encode_lower(
                &self.store.sealed_digest().ok_or("owner revision absent")?,
            ),
            pending,
        };
        if !next.valid() {
            return Err("owner transition invalid".into());
        }
        Ok(OwnerWitnessTransition {
            binding: journal.binding.clone(),
            expected: Some(expected),
            next,
            fresh_activation: false,
        })
    }

    fn persist(mut self) -> Result<Self, Box<OwnedRefreshRetention>> {
        let plaintext = self.payload().ok();
        let settled = plaintext.as_ref().is_some_and(|plaintext| {
            self.store.save(plaintext.as_slice()).is_ok()
                && aterm_durable::Flush::Durable.dir(&self.directory).is_ok()
        });
        if settled {
            Ok(self)
        } else {
            Err(Box::new(OwnedRefreshRetention {
                claimed: self,
                plaintext,
                raw: None,
                no_send: None,
                source_lease: None,
            }))
        }
    }

    fn retain_received(
        mut self,
        received: codex_app_login::ReceivedRefresh,
        expected: OwnerWitness,
    ) -> Result<Self, Box<OwnedRefreshRetention>> {
        let encoded = aterm_codec::base64::encode(received.body());
        let valid = self
            .journal
            .as_ref()
            .and_then(|journal| journal.pending.as_ref())
            .zip(expected.pending.as_ref())
            .is_some_and(|(pending, attempt)| {
                pending.phase == Phase::Attempted && pending_matches(pending, attempt)
            });
        let Ok(mut body) = encoded else {
            return Err(Box::new(OwnedRefreshRetention {
                claimed: self,
                plaintext: None,
                raw: Some((received, expected)),
                no_send: None,
                source_lease: None,
            }));
        };
        if !valid {
            body.zeroize();
            return Err(Box::new(OwnedRefreshRetention {
                claimed: self,
                plaintext: None,
                raw: Some((received, expected)),
                no_send: None,
                source_lease: None,
            }));
        }
        let journal = self.journal.as_mut().expect("validated refresh journal");
        let pending = journal.pending.as_mut().expect("validated refresh attempt");
        pending.response = Some(Response {
            status: received.status(),
            received_at: received.received_at(),
            body,
        });
        pending.phase = Phase::Responded;
        journal.transition = JournalTransition {
            expected: Some(expected),
            kind: TransitionKind::Response,
        };
        self.persist()
    }

    fn arm_for_send(mut self, expected: OwnerWitness) -> Result<Self, Box<OwnedRefreshRetention>> {
        let valid = self.journal.as_ref().is_some_and(|journal| {
            matches_revision(&journal.current, &expected.revision)
                && journal
                    .pending
                    .as_ref()
                    .zip(expected.pending.as_ref())
                    .is_some_and(|(pending, attempt)| {
                        pending.phase == Phase::Prepared
                            && pending.response.is_none()
                            && pending_matches(pending, attempt)
                    })
        });
        if !valid {
            return Err(Box::new(OwnedRefreshRetention {
                claimed: self,
                plaintext: None,
                raw: None,
                no_send: None,
                source_lease: None,
            }));
        }
        let journal = self.journal.as_mut().expect("validated send preparation");
        journal
            .pending
            .as_mut()
            .expect("validated send preparation")
            .phase = Phase::Attempted;
        journal.transition = JournalTransition {
            expected: Some(expected.clone()),
            kind: TransitionKind::Attempt,
        };
        self.persist().map_err(|mut retained| {
            retained.no_send = Some(expected);
            retained
        })
    }

    // Only a live executor's known no-send result permits this transition.
    // An Attempted marker recovered from disk provides no such knowledge.
    fn mark_not_sent(mut self, expected: OwnerWitness) -> Result<Self, Box<OwnedRefreshRetention>> {
        let valid = self.journal.as_ref().is_some_and(|journal| {
            matches_revision(&journal.current, &expected.revision)
                && journal
                    .pending
                    .as_ref()
                    .zip(expected.pending.as_ref())
                    .is_some_and(|(pending, attempt)| {
                        pending.response.is_none() && pending_matches(pending, attempt)
                    })
        });
        if !valid {
            return Err(Box::new(OwnedRefreshRetention {
                claimed: self,
                plaintext: None,
                raw: None,
                no_send: None,
                source_lease: None,
            }));
        }
        let journal = self.journal.as_mut().expect("validated not-sent proof");
        journal
            .pending
            .as_mut()
            .expect("validated not-sent proof")
            .phase = Phase::Prepared;
        journal.transition = JournalTransition {
            expected: Some(expected),
            kind: TransitionKind::NotSent,
        };
        self.persist()
    }
}

/// Read only a canonical-witness-bound revision. Missing mutable state never
/// reactivates the immutable issuance token. An authenticated transition may
/// expose the old canonical view while a worker reconciles its durable result.
pub(super) fn current_revision(
    env: &Env,
    binding: &GrantBinding,
    initial: &Record,
    witness: &OwnerWitness,
) -> Result<CredentialRevision, String> {
    if initial.request != binding.request || initial.phase != super::Phase::VerifiedResponse {
        return Err("initial owner revision unavailable".into());
    }
    let opened = Claimed::open(env, binding, false)?.ok_or("owner revision journal absent")?;
    opened.view(witness)
}

/// Initialize only under the actual fresh-publication authority. An existing
/// active journal is never reset, even when its credential bytes match.
#[cfg(test)]
pub(in crate::harness) fn initialize_revision(
    env: &Env,
    guard: &FreshActivationGuard<'_>,
) -> Result<OwnerWitnessTransition, String> {
    let binding = guard.binding();
    let document = guard.document();
    if document.is_empty() || document.len() > MAX_DOCUMENT {
        return Err("initial owner revision invalid".into());
    }
    let current = Current {
        identity: guard.identity().clone(),
        generation: 1,
        digest: aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(document)),
        document: aterm_codec::base64::encode(document)
            .map_err(|_| "initial owner revision unavailable")?,
    };
    current.open()?;
    let mut claimed = Claimed::open_with_fresh(env, binding, true, Some(guard))?
        .ok_or("initial owner revision unavailable")?;
    if let Some(existing) = claimed.journal.as_ref() {
        if existing.transition.kind != TransitionKind::Fresh
            || existing.transition.expected.is_some()
            || existing.current.generation != 1
            || existing.previous_current.is_some()
            || existing.pending.is_some()
            || !existing.completed.is_empty()
            || existing.current.identity != current.identity
            || existing.current.digest != current.digest
            || !aterm_digest::ct_eq(existing.current.open()?.document.as_slice(), document)
        {
            return Err("initial owner revision already advanced".into());
        }
    } else {
        claimed.journal = Some(Journal {
            schema: 3,
            binding: binding.clone(),
            current,
            previous_current: None,
            transition: JournalTransition {
                expected: None,
                kind: TransitionKind::Fresh,
            },
            pending: None,
            completed: Vec::new(),
        });
    }
    let plaintext = claimed.payload()?;
    // A prior write can have reached disk before its durability acknowledgement.
    // Adopt only our exact authenticated payload while retaining the same claim.
    if !claimed
        .store
        .recover_exact_write(&claimed.identity, plaintext.as_slice())?
    {
        claimed.store.save(plaintext.as_slice())?;
        aterm_durable::Flush::Durable
            .dir(&claimed.directory)
            .map_err(|_| "initial owner revision durability unavailable")?;
    }
    if !claimed
        .store
        .recover_exact_write(&claimed.identity, plaintext.as_slice())?
    {
        return Err("initial owner revision durability unavailable".into());
    }
    let journal = claimed
        .journal
        .as_ref()
        .ok_or("initial owner revision absent")?;
    let next = OwnerWitness {
        revision: slot::Revision {
            generation: 1,
            digest: journal.current.digest.clone(),
        },
        journal_digest: aterm_codec::hex::encode_lower(
            &claimed
                .store
                .sealed_digest()
                .ok_or("initial owner revision absent")?,
        ),
        pending: None,
    };
    if !next.valid() {
        return Err("initial owner revision invalid".into());
    }
    Ok(OwnerWitnessTransition {
        binding: binding.clone(),
        expected: None,
        next,
        fresh_activation: true,
    })
}

/// Only this journal module may mint a canonical witness mutation after proving
/// its exact durable payload. Serializable witness data is not this capability.
pub(in crate::harness) struct OwnerWitnessTransition {
    binding: GrantBinding,
    expected: Option<OwnerWitness>,
    next: OwnerWitness,
    fresh_activation: bool,
}
impl OwnerWitnessTransition {
    pub(in crate::harness) fn binding(&self) -> &GrantBinding {
        &self.binding
    }
    pub(in crate::harness) fn expected(&self) -> Option<&OwnerWitness> {
        self.expected.as_ref()
    }
    pub(in crate::harness) fn next(&self) -> &OwnerWitness {
        &self.next
    }
    pub(in crate::harness) fn is_fresh_activation(&self) -> bool {
        self.fresh_activation
    }
}

/// Owns the exact completed response, its signed plaintext and its exclusive
/// journal claim until encrypted settlement. It cannot initiate another HTTP
/// request and must remain retained by the app's counted operation.
#[must_use = "retain custody until encrypted settlement; retry never sends an HTTP request"]
pub(in crate::harness) struct OwnedRefreshRetention {
    claimed: Claimed,
    plaintext: Option<SecretBytes>,
    raw: Option<(codex_app_login::ReceivedRefresh, OwnerWitness)>,
    no_send: Option<OwnerWitness>,
    source_lease: Option<crate::harness::codex_source_gate::SourceWorkLease>,
}
impl std::fmt::Debug for OwnedRefreshRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedRefreshRetention(redacted)")
    }
}
pub(in crate::harness) enum OwnerRefreshError {
    Unavailable,
    Retaining(Box<OwnedRefreshRetention>),
}
impl std::fmt::Debug for OwnerRefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "OwnerRefreshError(unavailable)",
            Self::Retaining(_) => "OwnerRefreshError(retaining response, redacted)",
        })
    }
}
impl OwnedRefreshRetention {
    pub(in crate::harness) fn retain_source(
        mut self: Box<Self>,
        lease: crate::harness::codex_source_gate::SourceWorkLease,
    ) -> Box<Self> {
        self.source_lease = Some(lease);
        self
    }

    fn continued(
        result: Result<Claimed, Box<Self>>,
        source_lease: Option<crate::harness::codex_source_gate::SourceWorkLease>,
    ) -> Result<OwnerRefreshContinuation, Box<Self>> {
        match result {
            Ok(claimed) => Box::new(Self {
                claimed,
                plaintext: None,
                raw: None,
                no_send: None,
                source_lease,
            })
            .settled(),
            Err(mut retention) => {
                retention.source_lease = source_lease;
                Err(retention)
            }
        }
    }

    fn settled(self: Box<Self>) -> Result<OwnerRefreshContinuation, Box<Self>> {
        let Some(journal) = self.claimed.journal.as_ref() else {
            return Err(self);
        };
        Ok(OwnerRefreshContinuation {
            binding: journal.binding.clone(),
            source_lease: self.source_lease,
        })
    }

    pub(in crate::harness) fn retry(
        mut self: Box<Self>,
    ) -> Result<OwnerRefreshContinuation, Box<Self>> {
        if let Some((received, expected)) = self.raw.take() {
            return Self::continued(
                self.claimed.retain_received(received, expected),
                self.source_lease,
            );
        }
        if self.plaintext.is_none() {
            self.plaintext = self.claimed.payload().ok();
        }
        let Some(plaintext) = self.plaintext.as_ref() else {
            return Err(self);
        };
        let settled = self
            .claimed
            .store
            .recover_exact_write(&self.claimed.identity, plaintext.as_slice())
            .and_then(|recovered| {
                if recovered {
                    Ok(())
                } else {
                    self.claimed.store.save(plaintext.as_slice())
                }
            })
            .and_then(|()| {
                aterm_durable::Flush::Durable
                    .dir(&self.claimed.directory)
                    .map_err(|_| "owner revision durability unknown")
            });
        if settled.is_ok() {
            if let Some(expected) = self.no_send.take() {
                return Self::continued(self.claimed.mark_not_sent(expected), self.source_lease);
            }
            self.settled()
        } else {
            Err(self)
        }
    }
}

/// App-owned recovery after durable response settlement. This carries no session
/// delivery authority and never sends a provider request. Keep source exclusion
/// until shared canonical progress is reconciled, even if the caller departed.
#[must_use = "reconcile shared credential progress before retiring the app operation"]
pub(in crate::harness) struct OwnerRefreshContinuation {
    binding: GrantBinding,
    source_lease: Option<crate::harness::codex_source_gate::SourceWorkLease>,
}
impl std::fmt::Debug for OwnerRefreshContinuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerRefreshContinuation(redacted)")
    }
}
impl OwnerRefreshContinuation {
    pub(in crate::harness) fn reconcile(
        self,
        env: &Env,
        scope: &crate::harness::account_work::Scope,
    ) -> Result<(), OwnerRefreshError> {
        if self
            .source_lease
            .as_ref()
            .is_some_and(|lease| lease.current().is_err())
        {
            return Err(OwnerRefreshError::Unavailable);
        }
        match transaction::reconcile_retained(env, &self.binding, scope) {
            Err(OwnerRefreshError::Retaining(mut retention)) => {
                if let Some(lease) = self.source_lease {
                    retention = retention.retain_source(lease);
                }
                Err(OwnerRefreshError::Retaining(retention))
            }
            result => result,
        }
    }
}
