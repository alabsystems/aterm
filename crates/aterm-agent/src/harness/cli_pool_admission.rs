// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live authority for one local pool renewal, independent of shared-root grants.

use super::*;

#[cfg(test)]
#[path = "cli_pool_admission_tests.rs"]
mod tests;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::harness) enum Location {
    Staging,
    Published,
    Default,
}

pub(super) struct Current {
    pub store: accounts::Store,
    pub row: accounts::Row,
    pub roster: PathBuf,
    pub own_dir: Option<String>,
    pub key: super::super::login_drive::RecoveryKey,
    pub workflow: accounts::WorkflowToken,
    pub staging: PathBuf,
    pub location: Location,
    pub now: i64,
}

/// Establish the default credential fence before capturing recovery authority.
/// Later diagnostic records then update an existing generation, rather than
/// creating one after a provider has already rotated the grant.
pub(super) fn prepare(
    store: &accounts::Store,
    row: &accounts::Row,
    roster: &Path,
    own_dir: Option<&str>,
    now: i64,
    captured: &accounts::Logins,
) -> Result<Option<accounts::Logins>, String> {
    if !matches!(store.cli(), Cli::Kimi | Cli::Agy) {
        return Err("this pool has no native recovery generation".into());
    }
    if captured.get(&row.label).is_some() {
        return Ok(None);
    }
    let parent = accounts::RenewalParent::capture(captured, &row.label);
    store.transact_logins_within(&accounts::LOCK_LADDER, now, &mut None, |_, logins| {
        shared_renewal::unmanaged(logins, row, None, false)?;
        parent.require_current(logins, &row.label)?;
        signin_policy_current(store.cli(), row, roster, own_dir)?;
        if !accounts::admissible_label(&row.label) {
            return Err("pool native renewal label is invalid".into());
        }
        logins.labels.entry(row.label.clone()).or_default();
        Ok(Vec::new())
    })?;
    store.load_logins(now).map(|(logins, _)| Some(logins))
}

pub(super) fn snapshot_current(
    row: &accounts::Row,
    state: &accounts::State,
    logins: &accounts::Logins,
    key: super::super::login_drive::RecoveryKey,
) -> Result<(), String> {
    let parent = accounts::RenewalParent::capture(logins, &row.label);
    if parent.recovery_scope(row, state, logins)? != key.0 {
        return Err("pool native renewal parent or authentication facts changed".into());
    }
    Ok(())
}

impl Current {
    fn check(&self, principal: Option<[u8; 32]>) -> Result<(), String> {
        if self.store.cli() != Cli::Kimi || self.row.held_by.is_some() {
            return Err("pool native renewal is not admitted".into());
        }
        let now = aterm_time::unix_secs()
            .and_then(|n| i64::try_from(n).ok())
            .ok_or("pool native renewal clock is unavailable")?
            .max(self.now);
        let records = self
            .store
            .read_account_records(Some(&self.roster), now)
            .map_err(|_| "pool native renewal records are unavailable")?;
        signin_policy_matches(&self.row, &records.raw_roster, self.own_dir.as_deref())?;
        if !records.raw_roster.row(&self.row.label).is_some_and(|row| {
            accounts::workflow_token_current(row, &records.logins, self.workflow)
        }) {
            return Err("pool native renewal workflow changed".into());
        }
        let current = records
            .roster
            .row(&self.row.label)
            .filter(|row| *row == &self.row)
            .ok_or("pool native renewal row changed")?;
        shared_renewal::unmanaged(&records.logins, current, principal, principal.is_some())?;
        snapshot_current(current, &records.state, &records.logins, self.key)?;
        aterm_uds::private_file::check_private_dir(&self.store.logins_dir())
            .and_then(|()| aterm_uds::private_file::check_private_dir(&self.staging))
            .map_err(|_| "pool native renewal staging is unavailable")?;
        let location_allowed = match self.location {
            Location::Published => {
                records
                    .logins
                    .get(&self.row.label)
                    .and_then(|binding| binding.dir.as_deref())
                    == self.staging.to_str()
                    && current.dir.as_deref() == self.staging.to_str()
            }
            Location::Default => {
                current.dir.is_none()
                    && records
                        .logins
                        .get(&self.row.label)
                        .is_none_or(|binding| binding.dir.is_none())
                    && self.store.default_home().as_deref() == Some(self.staging.as_path())
            }
            Location::Staging => self.staging.parent() == Some(self.store.logins_dir().as_path()),
        };
        if !location_allowed
            || accounts::retained_directory_owner_at(
                &records.logins,
                &records.roster.rows,
                &self.row.label,
                self.staging
                    .to_str()
                    .ok_or("pool native staging is not UTF-8")?,
                self.store.default_home().as_deref(),
            )?
            .is_some()
            || accounts::shared_staging_retained(
                &records.logins,
                self.staging
                    .to_str()
                    .ok_or("pool native staging is not UTF-8")?,
            )?
        {
            return Err("pool native renewal staging custody changed".into());
        }
        Ok(())
    }

    pub(super) fn admit(self) -> super::super::login_drive::RecoveryAdmission {
        super::super::login_drive::RecoveryAdmission::pool(self.key, move |principal| {
            self.check(principal)
        })
    }
}
