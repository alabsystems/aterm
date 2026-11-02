// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::{
    accounts::{Action, Record, Request},
    binding::Binding,
    bootstrap::Bootstrap,
    journal::Journal,
    roster::Role,
};
use ring::signature::{ED25519, Ed25519KeyPair, UnparsedPublicKey};

const SUCCESSION_DOMAIN: &[u8] = b"aterm/account-issuer-succession/1\0";

/// Ephemeral predecessor consent for one exact intent and successor. This is
/// not an execution grant or a replacement for the application's current
/// canonical writer-selection checks. It cannot be decoded from caller data.
pub struct AccountIssuerSuccession {
    predecessor: Bootstrap,
    original: Binding,
    successor: Binding,
    intent: [u8; 32],
    signature: Vec<u8>,
}

impl AccountIssuerSuccession {
    fn message(&self) -> Vec<u8> {
        let mut message = SUCCESSION_DOMAIN.to_vec();
        message.extend_from_slice(&self.original.bytes());
        message.extend_from_slice(&self.successor.bytes());
        message.extend_from_slice(&self.predecessor.trust().genesis);
        message.extend_from_slice(&self.intent);
        message
    }

    fn check(
        &self,
        bootstrap: &Bootstrap,
        journal: &Journal,
        intent: &Record,
    ) -> crate::Result<()> {
        if self.original == self.successor {
            return Err(Error::new(
                "account issuer succession requires a different writer",
            ));
        }
        self.predecessor.revalidate_custody()?;
        let predecessor_journal =
            Journal::read_existing(self.predecessor.journal_dir(), self.predecessor.trust())?;
        let access = self
            .predecessor
            .access_in(&predecessor_journal)?
            .ok_or_else(|| Error::new("account issuer predecessor is unavailable"))?;
        let current = bootstrap
            .access_in(journal)?
            .ok_or_else(|| Error::new("account issuer successor is unavailable"))?;
        let Action::Intent(terms) = &intent.request().action else {
            return Err(Error::new("account issuer succession needs an intent"));
        };
        if intent.id() != self.intent
            || intent.trust() != journal.trust()
            || self.predecessor.trust() != journal.trust()
            || access.local().binding() != &terms.issuer
            || access.local().binding() != &self.original
            || access.local().role() != Role::Writer
            || !access.is_current(&predecessor_journal)
            || current.local().binding() != &self.successor
            || current.local().role() != Role::Writer
            || !current.is_current(journal)
            || !current
                .members()
                .iter()
                .any(|member| member.role() == Role::Writer && member.binding() == &self.original)
            || current
                .members()
                .iter()
                .filter(|member| {
                    member.role() == Role::Writer
                        && member.binding().machine() == self.successor.machine()
                })
                .count()
                != 1
        {
            return Err(Error::new("account issuer succession is no longer current"));
        }
        UnparsedPublicKey::new(&ED25519, terms.issuer.writer())
            .verify(&self.message(), &self.signature)
            .map_err(|_| Error::new("account issuer succession signature differs"))
    }
}

impl Admin {
    /// Authenticate predecessor consent after the application has verified its
    /// canonical completed writer lineage. The witness names exactly this
    /// intent and currently admitted successor, and must be rechecked at issue.
    pub fn account_issuer_succession(
        &self,
        predecessor: Bootstrap,
        current: &Bootstrap,
        journal: &Journal,
        intent: &Record,
    ) -> crate::Result<AccountIssuerSuccession> {
        if self.commons_authority()? != current.trust().authority {
            return Err(Error::new("account issuer succession has another root"));
        }
        let mut proof = AccountIssuerSuccession {
            original: predecessor.request()?.binding().clone(),
            predecessor,
            successor: current.request()?.binding().clone(),
            intent: intent.id(),
            signature: Vec::new(),
        };
        proof.signature = proof.predecessor.device().with_seed(|seed| {
            Ed25519KeyPair::from_seed_unchecked(seed)
                .map(|pair| pair.sign(&proof.message()).as_ref().to_vec())
                .map_err(|_| Error::new("invalid account issuer predecessor key"))
        })?;
        proof.check(current, journal, intent)?;
        Ok(proof)
    }

    /// Prepare root-signed account evidence for one exact current device. This
    /// does not publish it or establish its place in an account decision DAG.
    /// The application must durably retain these exact bytes before publishing.
    /// Only the authenticated published vault supplies credential bindings;
    /// mutable draft edits through `doc` cannot change them.
    pub fn prepare_account_decision(
        &self,
        bootstrap: &Bootstrap,
        journal: &Journal,
        request: Request,
    ) -> crate::Result<Record> {
        self.prepare_account_decision_in(bootstrap, journal, request, None, None)
    }

    /// Prepare one device decision under an independently checked current root
    /// intent. The application still checks the intent's retained DAG position.
    pub fn prepare_account_decision_with_intent(
        &self,
        bootstrap: &Bootstrap,
        journal: &Journal,
        request: Request,
        intent: &Record,
    ) -> crate::Result<Record> {
        self.prepare_account_decision_in(bootstrap, journal, request, Some(intent), None)
    }

    /// Sign a child only with exact predecessor consent, current root authority
    /// and the same credential checks as an ordinary intent child. The caller
    /// must also revalidate its canonical selected lineage before committing.
    pub fn prepare_account_decision_after_succession(
        &self,
        bootstrap: &Bootstrap,
        journal: &Journal,
        request: Request,
        intent: &Record,
        succession: &AccountIssuerSuccession,
    ) -> crate::Result<Record> {
        self.prepare_account_decision_in(
            bootstrap,
            journal,
            request,
            Some(intent),
            Some(succession),
        )
    }

    fn prepare_account_decision_in(
        &self,
        bootstrap: &Bootstrap,
        journal: &Journal,
        request: Request,
        intent: Option<&Record>,
        succession: Option<&AccountIssuerSuccession>,
    ) -> crate::Result<Record> {
        let (control, commitment) = self.account_context(bootstrap, journal, &request)?;
        if let Action::Prepare(preparation) = &request.action {
            match (preparation.intent, intent) {
                (Some(reference), Some(intent)) => {
                    let Action::Intent(terms) = &intent.request().action else {
                        return Err(Error::new("account preparation needs a root fleet intent"));
                    };
                    if intent.trust() != journal.trust()
                        || intent.request().account != reference.account
                        || intent.id() != reference.decision
                        || reference.device_account(&preparation.device) != request.account
                        || !terms.matches(preparation)
                    {
                        return Err(Error::new(
                            "account preparation differs from its fleet intent",
                        ));
                    }
                    let direct = bootstrap.access_in(journal)?.is_some_and(|access| {
                        let current = access.local().binding();
                        let writers = access
                            .members()
                            .iter()
                            .filter(|member| {
                                member.role() == Role::Writer
                                    && member.binding().machine() == current.machine()
                            })
                            .count();
                        terms.issued_by(current, writers)
                    });
                    if !direct {
                        succession
                            .ok_or_else(|| {
                                Error::new("fleet account has another automatic issuer")
                            })?
                            .check(bootstrap, journal, intent)?;
                    }
                    if let Some(commitment) = &commitment
                        && !intent
                            .matches_issuer_entry(&self.commons_seed, commitment.as_slice())?
                    {
                        return Err(Error::new(
                            "account credential changed after fleet approval",
                        ));
                    }
                }
                (None, None) => {}
                _ => return Err(Error::new("account preparation has no exact root intent")),
            }
        } else if intent.is_some() {
            return Err(Error::new(
                "only a device preparation may refer to a fleet intent",
            ));
        }
        let authored_ms = aterm_time::unix_millis()
            .ok_or_else(|| Error::new("clock cannot represent an author timestamp"))?;
        Record::issue(
            &self.commons_seed,
            journal.trust(),
            request,
            control,
            authored_ms,
            commitment.as_ref(),
        )
    }

    /// Verify current membership and the exact issuer-sealed credential binding
    /// before a recovery decision. No credential or fingerprint is returned.
    /// Current configuration and decision ancestry remain the application's job.
    pub fn account_binding_unchanged(
        &self,
        bootstrap: &Bootstrap,
        journal: &Journal,
        record: &Record,
    ) -> crate::Result<()> {
        if record.trust() != journal.trust() {
            return Err(Error::new("account binding has another trust context"));
        }
        let (_, commitment) = self.account_context(bootstrap, journal, record.request())?;
        if let Some(commitment) = commitment
            && !record.matches_issuer_entry(&self.commons_seed, commitment.as_slice())?
        {
            return Err(Error::new(
                "account credential no longer matches its root binding",
            ));
        }
        Ok(())
    }

    fn account_context(
        &self,
        bootstrap: &Bootstrap,
        journal: &Journal,
        request: &Request,
    ) -> crate::Result<(astream_wire::sync::Digest, Option<SecretBytes>)> {
        let trust = journal.trust();
        if trust != bootstrap.trust() || self.commons_authority()? != trust.authority {
            return Err(Error::new("account decision has another Commons authority"));
        }
        request.validate(trust)?;
        let access = bootstrap
            .access_in(journal)?
            .ok_or_else(|| Error::new("account decision needs current Commons membership"))?;
        if access.local().role() != Role::Writer {
            return Err(Error::new(
                "account decision issuer is not a current writer",
            ));
        }
        let mut commitment = None;
        if let Action::Prepare(preparation) = &request.action {
            if !access
                .members()
                .iter()
                .any(|m| m.binding() == &preparation.device && m.role() == Role::Writer)
            {
                return Err(Error::new(
                    "account preparation target is not a current writer",
                ));
            }
            let host = self
                .published_doc
                .machines()
                .into_iter()
                .find(|m| {
                    Recipient::parse(&m.recipient).ok() == Some(preparation.device.recipient())
                })
                .ok_or_else(|| {
                    Error::new("account preparation recipient is absent from the vault")
                })?;
            if !self
                .commons_eligibility
                .includes(&host.name, preparation.device.recipient())
            {
                return Err(Error::new(
                    "account preparation recipient is outside the control audience",
                ));
            }
            if let Some(reference) = &preparation.secret {
                self.check_account_secret(reference)?;
                let entry = text::merge_for(&host.name, self.published_doc.entries())
                    .map_err(|_| Error::new("account secret audience is ambiguous"))?
                    .into_iter()
                    .find(|e| e.name == reference.entry)
                    .ok_or_else(|| Error::new("account secret is not granted to this recipient"))?;
                commitment = Some(entry::fingerprint(&entry));
            }
        }
        if let Action::Intent(intent) = &request.action {
            if access.local().binding() != &intent.issuer {
                return Err(Error::new("fleet account intent has another issuer"));
            }
            if let Some(reference) = &intent.secret {
                self.check_account_secret(reference)?;
                // A fleet-wide approval must name one Everyone entry. Each device's
                // merged entry is checked against this sealed commitment on issue,
                // so a host override cannot silently substitute different bytes.
                let published = self.published_doc.entries();
                let mut entries = published.iter().filter(|entry| {
                    entry.name == reference.entry && entry.audience == Audience::Everyone
                });
                let entry = entries.next().ok_or_else(|| {
                    Error::new("fleet account credential is not granted to Everyone")
                })?;
                if entries.next().is_some() {
                    return Err(Error::new("fleet account credential is ambiguous"));
                }
                commitment = Some(entry::fingerprint(entry));
            }
        }
        let control = if access.is_log_backed() {
            access.basis()
        } else {
            access.head()
        };
        Ok((*control, commitment))
    }

    fn check_account_secret(
        &self,
        reference: &crate::commons::accounts::SecretRef,
    ) -> crate::Result<()> {
        if reference.vault != self.config.name
            || self.config.signer.is_none()
            || (self.config.signer != Some(reference.signer)
                && !self.config.followed.contains(&reference.signer)
                && self.custodian_signer().ok() != Some(reference.signer))
        {
            return Err(Error::new("account secret belongs to another vault"));
        }
        Ok(())
    }
}
