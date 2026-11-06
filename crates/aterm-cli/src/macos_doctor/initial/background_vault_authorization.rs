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
mod tests {
    use super::*;

    fn selected() -> Plan {
        let mut selection = Selection::none();
        selection.set(Setting::Secrets, true);
        Plan::from(selection)
    }

    fn admitted(root: &Path) -> Plan {
        let plan = selected();
        authorize_at(root, &plan).unwrap();
        plan
    }

    fn unchanged(root: &Path, operation: impl FnOnce()) {
        let before = observation_at(root).unwrap();
        operation();
        assert_eq!(observation_at(root).unwrap(), before);
    }

    #[test]
    fn an_automatic_default_with_no_records_has_read_only_authority() {
        let root = aterm_tempfile::tempdir().unwrap();
        let before = std::fs::read_dir(root.path()).unwrap().count();
        unchanged(root.path(), || {
            let authority = BackgroundVaultAuthorization::capture_at(root.path()).unwrap();
            authority.check_at().unwrap();
        });
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), before);
    }

    #[test]
    fn stop_restore_and_off_withdraw_the_original_background_proof() {
        for change in ["stop", "completed-stop", "restore", "off"] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut plan = admitted(root.path());
            if change == "completed-stop" {
                let mut progress = progress(root.path()).unwrap().unwrap();
                progress.done = Stage::Secrets.bit();
                progress.completed = true;
                save_progress(root.path(), progress).unwrap();
            }
            let authority = BackgroundVaultAuthorization::capture_at(root.path()).unwrap();
            authority.check_at().unwrap();
            match change {
                "stop" | "completed-stop" => {
                    request_stop_at(root.path(), revision_at(root.path()).unwrap()).unwrap()
                }
                "restore" => save_restore(root.path()).unwrap(),
                "off" => {
                    plan.selection.set(Setting::Secrets, false);
                    authorize_at(root.path(), &plan).unwrap();
                }
                _ => unreachable!(),
            }
            unchanged(root.path(), || {
                assert!(authority.check_at().is_err(), "{change}");
                assert!(
                    BackgroundVaultAuthorization::capture_at(root.path()).is_err(),
                    "{change}"
                );
            });
        }
    }

    #[test]
    fn a_new_allowed_revision_cannot_authorize_the_previous_proof() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = admitted(root.path());
        let authority = BackgroundVaultAuthorization::capture_at(root.path()).unwrap();
        let previous = revision_at(root.path()).unwrap();
        authorize_at(root.path(), &plan).unwrap();
        assert_ne!(revision_at(root.path()).unwrap(), previous);
        unchanged(root.path(), || {
            assert!(authority.check_at().is_err());
            BackgroundVaultAuthorization::capture_at(root.path())
                .unwrap()
                .check_at()
                .unwrap();
        });
    }

    #[test]
    fn source_changes_and_unreadable_records_never_lend_old_authority() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = admitted(root.path());
        let authority = BackgroundVaultAuthorization::capture_at(root.path()).unwrap();
        plan.vault_url = "https://chosen.example/v1/personal".into();
        authorize_at(root.path(), &plan).unwrap();
        unchanged(root.path(), || {
            assert!(authority.check_at().is_err());
            assert!(BackgroundVaultAuthorization::capture_at(root.path()).is_err());
        });
        std::fs::write(root.path().join("automatic-plan.json"), b"invalid").unwrap();
        let before = std::fs::read(root.path().join("automatic-plan.json")).unwrap();
        assert!(authority.check_at().is_err());
        assert!(BackgroundVaultAuthorization::capture_at(root.path()).is_err());
        assert_eq!(
            std::fs::read(root.path().join("automatic-plan.json")).unwrap(),
            before
        );
    }

    #[test]
    fn an_old_stage_or_identical_republication_cannot_lend_current_authority() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = admitted(root.path());
        let authority = BackgroundVaultAuthorization::capture_at(root.path()).unwrap();
        save_plan(root.path(), &plan).unwrap();
        assert!(
            authority.check_at().is_err(),
            "same bytes have another publication"
        );
        let mut changed = plan.clone();
        changed.selection.set(Setting::Repositories, true);
        save_plan(root.path(), &changed).unwrap();
        unchanged(root.path(), || {
            assert!(
                BackgroundVaultAuthorization::capture_at(root.path()).is_err(),
                "progress belongs to the previous plan"
            );
        });
        let mut progress = Progress::for_plan(&changed);
        progress.admitted = false;
        save_progress(root.path(), progress).unwrap();
        unchanged(root.path(), || {
            assert!(
                BackgroundVaultAuthorization::capture_at(root.path()).is_err(),
                "staging is not admission"
            );
        });
    }

    #[test]
    fn checking_never_takes_the_profile_lock() {
        let root = aterm_tempfile::tempdir().unwrap();
        admitted(root.path());
        let authority = BackgroundVaultAuthorization::capture_at(root.path()).unwrap();
        let _held = HeldLock::take(root.path()).unwrap();
        unchanged(root.path(), || authority.check_at().unwrap());
    }
}
