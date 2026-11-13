// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A background default-vault proof belongs to the exact setup observation that began it.

use super::*;

/// Read-only setup authority for one background default-ALab admission. Every publication
/// rechecks this observation; a later choice never lends its authority to the earlier proof.
/// This takes neither the profile lock nor the vault lock, including when checked while the
/// caller already holds the vault lock. It reserves no revision and writes no setup records.
pub struct BackgroundVaultAuthorization {
    root: PathBuf,
    observed: InitialSetupObservation,
}

impl BackgroundVaultAuthorization {
    /// Bind a background admission to this account's current default-vault setup.
    ///
    /// # Errors
    /// This is not the person's account, setup is held or changed, its source is custom, or
    /// the records cannot establish coherent current authority.
    pub fn capture() -> Result<Self, String> {
        ensure_person_scope()?;
        Self::capture_at(&state_root()?)
    }

    /// Check the same setup authority immediately before an admission's next effect.
    ///
    /// # Errors
    /// Account scope, setup choice, revision or publication changed, setup was stopped or
    /// restored, or the current records cannot be read. No setup or vault lock is acquired.
    pub fn check(&self) -> Result<(), String> {
        ensure_person_scope()?;
        if state_root()? != self.root {
            return Err("the background vault admission's account state changed".into());
        }
        self.check_at()
    }

    fn capture_at(root: &Path) -> Result<Self, String> {
        let observed = observation_at(root).map_err(|why| why.to_string())?;
        allowed(&observed.records)?;
        Ok(Self {
            root: root.to_path_buf(),
            observed,
        })
    }

    fn check_at(&self) -> Result<(), String> {
        let current = observation_at(&self.root).map_err(|why| why.to_string())?;
        allowed(&current.records)?;
        if current != self.observed {
            return Err("setup changed during the background vault admission".into());
        }
        Ok(())
    }
}

fn allowed(records: &Records) -> Result<(), String> {
    if holds(records).holds(Setting::Secrets) || matches!(records.decision, Decision::Stopped(_)) {
        return Err("current setup holds background vault admission".into());
    }
    match (&records.plan, records.progress) {
        (None, None) if records.stop_request.is_none() => Ok(()),
        (Some((plan, modern)), progress)
            if plan.selection.contains(Setting::Secrets)
                && aterm_secrets::is_alab_default_url(&plan.vault_url)
                && match progress {
                    Some(progress) => {
                        progress.admitted
                            && progress.belongs_to(plan)
                            && !progress.stopped
                            && !progress.named_by(records.stop_request)
                    }
                    None => !modern && records.stop_request.is_none(),
                } =>
        {
            Ok(())
        }
        _ => Err("current setup does not authorize background default-vault admission".into()),
    }
}

#[cfg(test)]
#[path = "background_vault_authorization_tests.rs"]
mod tests;
