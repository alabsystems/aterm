// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Canonical freshness evidence for the encrypted per-grant refresh journal.
//! These serializable values are correlation data, never a mutation capability.

use serde::{Deserialize, Serialize};

fn hex(word: &str, count: usize) -> bool {
    word.len() == count
        && word
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::harness) struct OwnerAttempt {
    pub(in crate::harness) operation: String,
    pub(in crate::harness) request_digest: String,
    pub(in crate::harness) base_generation: u64,
    pub(in crate::harness) base_digest: String,
    pub(in crate::harness) token_digest: String,
}
impl std::fmt::Debug for OwnerAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerAttempt(redacted)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::harness) struct OwnerWitness {
    pub(in crate::harness) revision: super::super::codex_session_slot::Revision,
    pub(in crate::harness) journal_digest: String,
    pub(in crate::harness) pending: Option<OwnerAttempt>,
}
impl std::fmt::Debug for OwnerWitness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerWitness(redacted)")
    }
}
impl OwnerWitness {
    pub(in crate::harness) fn valid(&self) -> bool {
        self.revision.generation > 0
            && hex(&self.revision.digest, 64)
            && hex(&self.journal_digest, 64)
            && self.pending.as_ref().is_none_or(|attempt| {
                hex(&attempt.operation, 32)
                    && hex(&attempt.request_digest, 64)
                    && attempt.base_generation == self.revision.generation
                    && attempt.base_digest == self.revision.digest
                    && hex(&attempt.token_digest, 64)
            })
    }
}

/// Non-serializable evidence produced only after the exact account-writer
/// transaction durably records this owner's contextual SignedOut result.
pub(in crate::harness) struct SettledRefusal {
    binding: super::super::codex_grant_owner::GrantBinding,
    witness: OwnerWitness,
    at: i64,
}
impl SettledRefusal {
    pub(in crate::harness) fn binding(&self) -> &super::super::codex_grant_owner::GrantBinding {
        &self.binding
    }
    pub(in crate::harness) fn witness(&self) -> &OwnerWitness {
        &self.witness
    }
    pub(in crate::harness) fn at(&self) -> i64 {
        self.at
    }
}

/// Canonical journal-witness CAS while the native admission keeps the existing
/// account writer. It never acquires another account lock or writes raw images.
pub(in crate::harness) struct OwnerAnchorWriter<'a> {
    store: &'a super::accounts::Store,
    held: &'a super::accounts::Lock,
    binding: &'a super::super::codex_grant_owner::GrantBinding,
    policy: super::accounts::RenewalPolicy<'a>,
    now: i64,
}
impl<'a> OwnerAnchorWriter<'a> {
    pub(in crate::harness) fn new(
        store: &'a super::accounts::Store,
        held: &'a super::accounts::Lock,
        binding: &'a super::super::codex_grant_owner::GrantBinding,
        policy: super::accounts::RenewalPolicy<'a>,
        now: i64,
    ) -> Self {
        Self {
            store,
            held,
            binding,
            policy,
            now,
        }
    }

    pub(in crate::harness) fn read(&self) -> Result<Option<OwnerWitness>, String> {
        let (logins, warning) = self.store.load_logins(self.now)?;
        if warning.is_some() {
            return Err("owner anchor unavailable".into());
        }
        logins.enrollment.owner_witness(self.binding)
    }

    pub(in crate::harness) fn compare_exchange(
        &mut self,
        transition: &super::super::codex_grant_owner::OwnerWitnessTransition,
    ) -> Result<(), String> {
        if transition.binding() != self.binding {
            return Err("owner anchor belongs to another grant".into());
        }
        self.store
            .transact_logins_held(self.held, self.now, &mut None, |_, logins| {
                logins.enrollment.cas_owner_witness(transition)?;
                Ok(Vec::new())
            })?;
        Ok(())
    }

    /// Record a settled refresh refusal against the same effective row and credential context
    /// that canonical owner admission authenticated. The owner witness check and contextual auth
    /// word share the retained account writer, so a changed grant, roster or login writes none.
    pub(in crate::harness) fn record_settled_refusal(
        &mut self,
        expected: &OwnerWitness,
        at: i64,
    ) -> Result<SettledRefusal, String> {
        if at <= 0 || at > self.now {
            return Err("settled refusal time unavailable".into());
        }
        let context = self
            .store
            .capture_auth_context(self.policy.row)
            .ok_or("rejected owner credential context unavailable")?;
        let raw = self
            .policy
            .roster
            .row(&self.binding.request.label)
            .ok_or("rejected owner row unavailable")?;
        let expected_context = context.clone();
        self.store.transact_renewal_held(
            self.held,
            self.now,
            &mut None,
            self.policy,
            |state, logins| {
                if logins.enrollment.owner_witness(self.binding)?.as_ref() != Some(expected) {
                    return Err("rejected owner witness changed".into());
                }
                super::accounts::stage_auth(
                    state,
                    raw,
                    logins,
                    context,
                    super::accounts::Auth::SignedOut,
                    at,
                );
                Ok(Vec::new())
            },
        )?;
        // A newer auth observation can make the account transaction deliberately
        // ignore this timestamp. Only the actual durable contextual SignedOut
        // word at or after the settled boundary, not merely a successful
        // transaction, creates retirement evidence. Keep the boundary itself
        // stable when a newer observation already satisfies the sign-out.
        let (state, warning) = self.store.load(self.now)?;
        if warning.is_some()
            || !state
                .accounts
                .get(&self.binding.request.label)
                .is_some_and(|account| {
                    account.auth == super::accounts::Auth::SignedOut
                        && account
                            .auth_at
                            .is_some_and(|observed| observed >= at && observed <= self.now)
                        && account.auth_context.as_ref() == Some(&expected_context)
                })
            || self.store.capture_auth_context(self.policy.row).as_ref() != Some(&expected_context)
        {
            return Err("settled refusal sign-out was not durably recorded".into());
        }
        Ok(SettledRefusal {
            binding: self.binding.clone(),
            witness: expected.clone(),
            at,
        })
    }

    pub(in crate::harness) fn retire_catalogue(
        &mut self,
        env: &super::super::cli::Env,
        proof: &SettledRefusal,
    ) -> Result<(), String> {
        if proof.binding() != self.binding || self.read()?.as_ref() != Some(proof.witness()) {
            return Err("settled refusal owner changed".into());
        }
        let (logins, warning) = self.store.load_logins(self.now)?;
        if warning.is_some() {
            return Err("settled refusal account unavailable".into());
        }
        if !logins
            .enrollment
            .catalogue_required_id(&self.binding.request.id)
        {
            return Ok(());
        }
        let staging = logins.enrollment.catalogue_staging(&self.binding.request)?;
        if std::fs::canonicalize(&self.store.dir).ok().as_ref() != Some(&staging.store)
            || !staging.store_current()
        {
            return Err("settled refusal source changed".into());
        }
        let mut anchor = super::CatalogueAnchorWriter::new(
            self.store,
            self.held,
            &self.binding.request,
            self.now,
        );
        super::super::codex_grant_owner::retire_settled_refusal(env, proof, &mut anchor)
    }
}
