// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::{
    accounts::{Action, Record, Request},
    bootstrap::Bootstrap,
    journal::Journal,
    roster::Role,
};

impl Admin {
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
                if reference.vault != self.config.name
                    || self.config.signer.is_none()
                    || (self.config.signer != Some(reference.signer)
                        && !self.config.followed.contains(&reference.signer)
                        && self.custodian_signer().ok() != Some(reference.signer))
                {
                    return Err(Error::new("account secret belongs to another vault"));
                }
                let entry = text::merge_for(&host.name, self.published_doc.entries())
                    .map_err(|_| Error::new("account secret audience is ambiguous"))?
                    .into_iter()
                    .find(|e| e.name == reference.entry)
                    .ok_or_else(|| Error::new("account secret is not granted to this recipient"))?;
                commitment = Some(entry::fingerprint(&entry));
            }
        }
        let control = if access.is_log_backed() {
            access.basis()
        } else {
            access.head()
        };
        let authored_ms = aterm_time::unix_millis()
            .ok_or_else(|| Error::new("clock cannot represent an author timestamp"))?;
        Record::issue(
            &self.commons_seed,
            trust,
            request,
            *control,
            authored_ms,
            commitment.as_ref(),
        )
    }
}
