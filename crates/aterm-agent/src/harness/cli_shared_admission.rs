// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live root authority for one exact, consumed native attempt.

use super::*;

pub(super) struct Current {
    pub authority: super::super::super::accounts_sync::shared_renewal::CheckedAuthority,
    pub grant: shared_renewal::Grant,
    pub row: accounts::Row,
    pub store: accounts::Store,
    pub roster: PathBuf,
    pub target: PathBuf,
    pub before: Option<String>,
    pub rows: Vec<accounts::Row>,
    pub intent: Intent,
    pub now: i64,
}

impl Current {
    fn check(&self, principal: Option<[u8; 32]>) -> Result<(), String> {
        if self.grant.source != self.intent.source
            || self.grant.decision != self.intent.decision
            || self.grant.generation != self.intent.generation
            || self.grant.configuration != self.intent.configuration
            || self.row.label != self.intent.label
            || !accounts::email_matches(&self.grant.identity, &self.intent.identity)
            || !self
                .row
                .expected_identity_email()
                .is_some_and(|email| accounts::email_matches(email, &self.intent.identity))
            || self.intent.before_generation.is_none()
        {
            return Err("shared native admission differs from its retained intent".into());
        }
        if !super::super::super::accounts_sync::shared_adoption_on(&self.roster)
            || accounts::roster_text(&self.target)? != self.before
        {
            return Err("shared native admission policy changed".into());
        }
        if !private_staging_directory(&self.store.logins_dir())?
            || !private_staging_directory(Path::new(&self.intent.staging))?
        {
            return Err("shared native admission staging is unavailable".into());
        }
        self.authority.current(&self.grant, &self.row)?;
        let logins = self.store.load_logins(self.now)?.0;
        if logins
            .shared
            .latest(self.grant.source)
            .is_none_or(|attempt| {
                attempt.intent != self.intent || attempt.outcome != Outcome::Pending
            })
            || logins
                .get(&self.row.label)
                .map(|binding| binding.renewal_generation)
                != self.intent.before_generation
            || accounts::retained_directory_owner(
                &logins,
                &self.rows,
                &self.row.label,
                &self.intent.staging,
            )?
            .is_some()
        {
            return Err("shared native admission custody changed".into());
        }
        if let Some(principal) = principal
            && (self
                .grant
                .expected_principal
                .is_some_and(|expected| expected != principal)
                || !logins
                    .shared
                    .principal_allowed(self.grant.source, principal))
        {
            return Err("shared native admission principal changed".into());
        }
        Ok(())
    }

    pub(super) fn admit(
        self,
        native_entry: bool,
    ) -> super::super::super::login_drive::RecoveryAdmission {
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm.shared-native-recovery.v1\0");
        for field in [
            self.intent.source.network.as_slice(),
            self.intent.source.account.as_slice(),
            self.intent.decision.as_slice(),
            self.intent.configuration.as_slice(),
            self.intent.request.as_slice(),
            self.intent.label.as_bytes(),
            self.intent.identity.as_bytes(),
            self.intent.staging.as_bytes(),
            self.store.dir.as_os_str().as_encoded_bytes(),
        ] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field);
        }
        hash.update(self.intent.generation.to_be_bytes());
        hash.update(self.intent.before_generation.unwrap_or(0).to_be_bytes());
        hash.update(self.intent.begun_at.to_be_bytes());
        super::super::super::login_drive::RecoveryAdmission::new(
            super::super::super::login_drive::RecoveryKey(hash.finalize()),
            native_entry,
            move |principal| self.check(principal),
        )
    }
}
