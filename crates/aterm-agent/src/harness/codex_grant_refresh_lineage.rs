// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only alias evidence from a MACed revision journal and its canonical
//! high-water witness. Token hashes here confer no refresh/delivery permission.
use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::harness::codex_grant_owner) enum LineageUncertainty {
    None,
    BoundManagedAttempt,
    Global,
}

pub(in crate::harness) struct LineageHistory {
    pub(in crate::harness) tokens: BTreeSet<String>,
    pub(in crate::harness) pending: bool,
    pub(in crate::harness::codex_grant_owner) uncertainty: LineageUncertainty,
}
impl std::fmt::Debug for LineageHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LineageHistory(redacted)")
    }
}
pub(in crate::harness::codex_grant_owner) fn lineage_history(
    env: &Env,
    binding: &GrantBinding,
    initial: &Record,
    witness: &OwnerWitness,
) -> Result<LineageHistory, String> {
    if initial.request != binding.request || initial.phase != super::super::Phase::VerifiedResponse
    {
        return Err("lineage issuer unavailable".into());
    }
    let observed = JournalObservation::read(env, binding, witness)?;
    let visible = &observed.visible;
    let expected = initial
        .response
        .as_ref()
        .and_then(|response| response.identity.as_ref())
        .ok_or("lineage identity absent")?;
    if visible.identity != *expected {
        return Err("lineage identity changed".into());
    }
    let journal = &observed.journal;
    let mut tokens = BTreeSet::new();
    tokens.insert(codex_app_login::owned_refresh_token_digest(
        visible.document.as_slice(),
    )?);
    tokens.insert(codex_app_login::owned_refresh_token_digest(
        journal.current.open()?.document.as_slice(),
    )?);
    if let Some(previous) = &journal.previous_current {
        tokens.insert(codex_app_login::owned_refresh_token_digest(
            previous.open()?.document.as_slice(),
        )?);
    }
    for completed in &journal.completed {
        if let Some(token) = &completed.spent_token_digest {
            tokens.insert(token.clone());
        }
    }
    let mut uncertainty = LineageUncertainty::None;
    if let Some(pending) = &journal.pending {
        if let Some(token) = &pending.token_digest {
            tokens.insert(token.clone());
        }
        if let Some(response) = &pending.response {
            let base = journal
                .previous_current
                .as_ref()
                .unwrap_or(&journal.current)
                .open()?;
            let body = response.bytes()?;
            let candidate = response
                .received_at
                .ok_or_else(|| "lineage receipt time unavailable".to_owned())
                .and_then(|at| {
                    codex_app_login::merge_owned_refresh(
                        base.document.as_slice(),
                        response.status,
                        body.as_slice(),
                        at,
                    )
                });
            match candidate {
                Ok(document) => {
                    tokens.insert(codex_app_login::owned_refresh_token_digest(
                        document.as_slice(),
                    )?);
                }
                // A terminal answer is confined to this authenticated principal. A refusal
                // issued no successor; a quarantined unusable answer may have issued one, but
                // any such successor still belongs to this principal, which the catalogue
                // continues to fence. While the answer is merely Responded, reconciliation
                // has not made that terminal classification and its effect remains global.
                Err(_) => {
                    if !codex_app_login::owned_refresh_refused(response.status, body.as_slice())
                        && pending.phase != Phase::Quarantined
                    {
                        uncertainty = LineageUncertainty::Global;
                    }
                }
            }
        }
        if pending.phase == Phase::Attempted {
            // JournalObservation has joined the complete MACed body and actual
            // canonical OwnerAttempt. Only this managed base is scoped; an
            // unbound Code attempt never enters this projection.
            if pending.response.is_none()
                && pending.token_digest.as_deref()
                    == Some(
                        codex_app_login::owned_refresh_token_digest(
                            journal.current.open()?.document.as_slice(),
                        )?
                        .as_str(),
                    )
                && pending.base_generation == journal.current.generation
                && pending.base_digest == journal.current.digest
            {
                uncertainty = LineageUncertainty::BoundManagedAttempt;
            } else {
                uncertainty = LineageUncertainty::Global;
            }
        }
    }
    observed.current()?;
    Ok(LineageHistory {
        tokens,
        // Rejected and Quarantined are terminal for this grant. Their retained response and
        // principal/token facts still fence aliases, but neither holds another grant's code
        // exchange or publication behind work that can no longer make progress.
        pending: journal
            .pending
            .as_ref()
            .is_some_and(|pending| !matches!(pending.phase, Phase::Rejected | Phase::Quarantined)),
        uncertainty,
    })
}

/// No Profile claim: inventory must never wait on the other owner's live HTTP
/// executor. MAC + canonical witness authenticate the complete bounded snapshot.
struct JournalObservation {
    path: PathBuf,
    cipher_digest: [u8; 32],
    journal: Journal,
    visible: CredentialRevision,
}
impl JournalObservation {
    fn read(env: &Env, binding: &GrantBinding, witness: &OwnerWitness) -> Result<Self, String> {
        let (owner, home) = Store::location(env, &binding.request.id)?;
        let directory = owner.join("refresh");
        platform::check_private_dir(&directory).map_err(|_| "lineage journal unavailable")?;
        let identity = platform::machine_identity(&home)
            .map_err(|_| "lineage machine identity unavailable")?
            .ok_or("lineage machine identity unavailable")?;
        let ciphertext =
            platform::read_single_link_private(&directory, "candidate.age", MAX_JOURNAL)
                .map_err(|_| "lineage journal unavailable")?;
        let cipher_digest = aterm_digest::Sha256::digest(ciphertext.as_slice());
        let bytes = age::decrypt(std::slice::from_ref(&identity), ciphertext.as_slice())
            .map_err(|_| "lineage journal unavailable")?;
        let envelope: Envelope =
            aterm_json::from_slice(bytes.as_slice()).map_err(|_| "lineage journal invalid")?;
        let journal = envelope.journal;
        if !journal.valid()
            || journal.binding != *binding
            || !valid_hex(&envelope.mac, 64)
            || !aterm_digest::ct_eq(
                envelope.mac.as_bytes(),
                signature(&identity, &journal)?.as_bytes(),
            )
        {
            return Err("lineage journal invalid".into());
        }
        let exact = witness.journal_digest == aterm_codec::hex::encode_lower(&cipher_digest);
        let descendant = journal.transition.expected.as_ref() == Some(witness);
        if !witness.valid()
            || (!exact && !descendant)
            || (exact && witness.pending != journal_pending(&journal)?)
        {
            return Err("lineage journal is not canonically anchored".into());
        }
        let visible = if matches_revision(&journal.current, &witness.revision) {
            journal.current.open()?
        } else if !exact && descendant {
            journal
                .previous_current
                .as_ref()
                .filter(|previous| matches_revision(previous, &witness.revision))
                .ok_or("lineage journal requires reconciliation")?
                .open()?
        } else {
            return Err("lineage journal requires reconciliation".into());
        };
        let observed = Self {
            path: directory,
            cipher_digest,
            journal,
            visible,
        };
        observed.current()?;
        Ok(observed)
    }
    fn current(&self) -> Result<(), String> {
        let bytes = platform::read_single_link_private(&self.path, "candidate.age", MAX_JOURNAL)
            .map_err(|_| "lineage journal unavailable")?;
        if aterm_digest::Sha256::digest(bytes.as_slice()) != self.cipher_digest {
            return Err("lineage journal advanced during inventory".into());
        }
        Ok(())
    }
}

/// Real exclusion, not delivery authority. Acquire before the account writer;
/// the actual admitted callback must match its exact current source again.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LineagePurpose {
    Send,
    Settlement,
}

pub(in crate::harness::codex_grant_owner) struct RefreshLineageLease {
    binding: GrantBinding,
    identity: Identity,
    revision: slot::Revision,
    tokens: BTreeSet<String>,
    resources: crate::harness::native_enrollment_admission::RefreshResourceTurn,
    purpose: LineagePurpose,
}
/// Inventory observation only. Its private factory requires actual held
/// lineage resources; raw bindings/digests cannot construct one.
pub(in crate::harness::codex_grant_owner) struct OwnedRefreshInventorySource<'a> {
    lease: &'a RefreshLineageLease,
    binding: &'a GrantBinding,
    identity: &'a Identity,
    document: &'a [u8],
}
impl OwnedRefreshInventorySource<'_> {
    pub(in crate::harness::codex_grant_owner) fn settling(&self) -> bool {
        self.lease.purpose == LineagePurpose::Settlement
    }

    pub(in crate::harness::codex_grant_owner) fn binding(&self) -> &GrantBinding {
        self.binding
    }
    pub(in crate::harness::codex_grant_owner) fn identity(&self) -> &Identity {
        self.identity
    }
    pub(in crate::harness::codex_grant_owner) fn document(&self) -> &[u8] {
        self.document
    }
    pub(in crate::harness::codex_grant_owner) fn current(&self) -> Result<(), String> {
        self.lease
            .covers(self.binding, self.identity, self.document)
    }
}

impl RefreshLineageLease {
    pub(super) fn for_send(
        env: &Env,
        binding: &GrantBinding,
        work: &super::transaction::RefreshWorkLease,
    ) -> Result<Self, String> {
        Self::for_binding(env, binding, work, LineagePurpose::Send)
    }
    pub(super) fn for_reconciliation(
        env: &Env,
        binding: &GrantBinding,
        work: &super::transaction::RefreshWorkLease,
    ) -> Result<Self, String> {
        Self::for_binding(env, binding, work, LineagePurpose::Settlement)
    }
    fn for_binding(
        env: &Env,
        binding: &GrantBinding,
        work: &super::transaction::RefreshWorkLease,
        purpose: LineagePurpose,
    ) -> Result<Self, String> {
        let reconciliation = purpose == LineagePurpose::Settlement;
        work.current()?;
        let store = env
            .accounts_store_for(crate::harness::accounts::Cli::Codex)
            .ok_or("lineage source unavailable")?;
        let (logins, warning) = store.load_logins(env.now)?;
        if warning.is_some() {
            return Err("lineage source uncertain".into());
        }
        let state = logins.enrollment.catalogue_record_state(&binding.request)?;
        if state.published.as_ref() != Some(binding) {
            return Err("lineage canonical grant changed".into());
        }
        let (owner, initial) =
            super::super::open_authenticated(env, &binding.request, &binding.reference)?;
        let witness = state
            .owner_witness
            .as_ref()
            .ok_or("lineage owner witness absent")?;
        let observed = JournalObservation::read(env, binding, witness)?;
        let current = &observed.visible;
        let pending = observed.journal.pending.as_ref();
        if !reconciliation
            && pending.is_some_and(|pending| {
                pending.phase != Phase::Prepared || pending.response.is_some()
            })
        {
            return Err("lineage refresh requires prior response reconciliation".into());
        }
        if initial
            .response
            .as_ref()
            .and_then(|response| response.identity.as_ref())
            != Some(&current.identity)
        {
            return Err("lineage current principal changed".into());
        }
        let mut proof = super::super::response_resource_identity(
            &binding.request,
            &current.identity,
            current.document.as_slice(),
        )?;
        let mut tokens = BTreeSet::from([codex_app_login::owned_refresh_token_digest(
            current.document.as_slice(),
        )?]);
        if reconciliation {
            let journal = &observed.journal;
            for document in [
                Some(journal.current.open()?.document),
                journal
                    .previous_current
                    .as_ref()
                    .map(Current::open)
                    .transpose()?
                    .map(|previous| previous.document),
            ]
            .into_iter()
            .flatten()
            {
                let candidate = super::super::response_resource_identity(
                    &binding.request,
                    &current.identity,
                    document.as_slice(),
                )?;
                proof.resources.extend(candidate.resources);
                tokens.insert(codex_app_login::owned_refresh_token_digest(
                    document.as_slice(),
                )?);
            }
            if let Some(response) = pending.and_then(|pending| pending.response.as_ref()) {
                let base = journal
                    .previous_current
                    .as_ref()
                    .unwrap_or(&journal.current)
                    .open()?;
                let body = response.bytes()?;
                if let Some(at) = response.received_at
                    && let Ok(document) = codex_app_login::merge_owned_refresh(
                        base.document.as_slice(),
                        response.status,
                        body.as_slice(),
                        at,
                    )
                {
                    let candidate = super::super::response_resource_identity(
                        &binding.request,
                        &current.identity,
                        document.as_slice(),
                    )?;
                    proof.resources.extend(candidate.resources);
                    tokens.insert(codex_app_login::owned_refresh_token_digest(
                        document.as_slice(),
                    )?);
                }
            }
            proof.resources.sort();
            proof.resources.dedup();
        }
        let identity = current.identity.clone();
        let revision = current.revision.clone();
        observed.current()?;
        owner.current()?;
        drop(owner);
        let namespace = crate::harness::codex_source_gate::existing_namespace_for_home(
            env.home.as_deref().ok_or("lineage home unavailable")?,
        )
        .map_err(str::to_owned)?;
        let resources =
            crate::harness::native_enrollment_admission::RefreshResourceTurn::try_acquire(
                namespace, &proof,
            )?
            .ok_or("lineage resource busy")?;
        work.current()?;
        let held = Self {
            binding: binding.clone(),
            identity,
            revision,
            tokens,
            resources,
            purpose,
        };
        match held.assess_catalogue(env, binding, &current.identity, current.document.as_slice())? {
            super::super::catalogue::RefreshSourceStatus::NoKnownAlias => {}
            super::super::catalogue::RefreshSourceStatus::Conflicted if reconciliation => {
                // Settlement may durably quarantine a received alias; it gains
                // no permission to send or export that candidate.
            }
            super::super::catalogue::RefreshSourceStatus::Conflicted => {
                return Err("lineage alias is unresolved".into());
            }
        }
        work.current()?;
        Ok(held)
    }
    pub(super) fn assess_catalogue(
        &self,
        env: &Env,
        binding: &GrantBinding,
        identity: &Identity,
        document: &[u8],
    ) -> Result<super::super::catalogue::RefreshSourceStatus, String> {
        self.covers(binding, identity, document)?;
        super::super::catalogue::assess_owned_refresh(
            env,
            &OwnedRefreshInventorySource {
                lease: self,
                binding,
                identity,
                document,
            },
        )
    }
    pub(super) fn check_catalogue(
        &self,
        env: &Env,
        binding: &GrantBinding,
        identity: &Identity,
        document: &[u8],
    ) -> Result<(), String> {
        match self.assess_catalogue(env, binding, identity, document)? {
            super::super::catalogue::RefreshSourceStatus::NoKnownAlias => Ok(()),
            super::super::catalogue::RefreshSourceStatus::Conflicted => {
                Err("lineage alias is unresolved".into())
            }
        }
    }
    pub(super) fn matches(
        &self,
        binding: &GrantBinding,
        identity: &Identity,
        revision: &slot::Revision,
        document: &[u8],
    ) -> Result<(), String> {
        self.covers(binding, identity, document)?;
        if self.purpose != LineagePurpose::Send || &self.revision != revision {
            return Err("lineage source advanced before provider use".into());
        }
        Ok(())
    }
    pub(super) fn covers(
        &self,
        binding: &GrantBinding,
        identity: &Identity,
        document: &[u8],
    ) -> Result<(), String> {
        self.resources.current()?;
        if &self.binding != binding
            || &self.identity != identity
            || !self
                .tokens
                .contains(&codex_app_login::owned_refresh_token_digest(document)?)
        {
            return Err("lineage candidate lacks held resource custody".into());
        }
        Ok(())
    }
    pub(super) fn current(&self) -> Result<(), String> {
        self.resources.current()
    }
}
