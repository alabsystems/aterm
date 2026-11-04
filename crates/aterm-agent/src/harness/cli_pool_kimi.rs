// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Renewal of a published Kimi profile in place: its refresh grant stays in the
//! same native lock domain used by CLI processes already serving the account.

use super::*;
use shared_provider::ObservationError;

pub(super) enum Outcome {
    Renewed,
    Reauthenticate(shared_provider::KimiRejectedSnapshot),
}

pub(super) struct Attempt<'a> {
    pub plan: &'a SignInPlan,
    pub store: &'a accounts::Store,
    pub row: &'a accounts::Row,
    pub roster: &'a Path,
    pub own_dir: Option<&'a str>,
    pub now: i64,
}

#[derive(Debug)]
struct Guard(super::super::login_drive::RecoveryAdmission);
impl aterm_http::Guard for Guard {
    fn is_authorized(&self) -> bool {
        self.0.current(None).is_ok()
    }
}

fn unavailable(why: impl Into<String>) -> ObservationError {
    ObservationError::Unavailable(why.into())
}

/// A candidate path is scheduling evidence only; it grants no credential use.
pub(in crate::harness) fn profile(
    store: &accounts::Store,
    row: &accounts::Row,
    logins: &accounts::Logins,
) -> Result<Option<(PathBuf, pool_admission::Location)>, String> {
    if let Some(dir) = logins
        .get(&row.label)
        .and_then(|binding| binding.dir.as_deref())
    {
        return Ok(Some((dir.into(), pool_admission::Location::Published)));
    }
    if row.dir.is_some() || row.expected_identity_email().is_none() {
        return Ok(None);
    }
    let (Some(home), Some(region)) = (store.default_home(), row.vendor.region) else {
        return Ok(None);
    };
    for directory in [&home, &home.join("credentials")] {
        match std::fs::symlink_metadata(directory) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err("Kimi default profile directory is not a real directory".into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Kimi default profile directory is unavailable".into()),
        }
    }
    let credential = shared_provider::kimi_credential_path(&home, region);
    match std::fs::symlink_metadata(credential) {
        Ok(meta) if meta.is_file() => Ok(Some((home, pool_admission::Location::Default))),
        Ok(_) => Err("Kimi default credential is not a regular file".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("Kimi default credential is unavailable".into()),
    }
}

impl Attempt<'_> {
    pub(super) fn renew(
        &self,
        host: &dyn AccountsHost,
    ) -> Result<Option<Outcome>, ObservationError> {
        if !host.renews_kimi_profiles() {
            return Ok(None);
        }
        let records = self
            .store
            .read_account_records(Some(self.roster), self.now)
            .map_err(|_| unavailable("Kimi published account records are unavailable"))?;
        let Some((path, location)) =
            profile(self.store, self.row, &records.logins).map_err(unavailable)?
        else {
            return Ok(None);
        };
        let dir = path
            .to_str()
            .ok_or_else(|| unavailable("Kimi native profile is not UTF-8"))?;
        let before = records
            .logins
            .get(&self.row.label)
            .and_then(|binding| binding.dir.clone());
        let region = self
            .row
            .vendor
            .region
            .ok_or_else(|| unavailable("Kimi profile region is absent"))?;
        let expected = self
            .row
            .expected_identity_email()
            .ok_or_else(|| unavailable("Kimi profile identity is absent"))?;
        let key = self
            .plan
            .native_recovery
            .ok_or_else(|| unavailable("Kimi profile recovery authority is absent"))?;
        let admission = pool_admission::Current {
            store: self.store.clone(),
            row: self.row.clone(),
            roster: self.roster.to_owned(),
            own_dir: self.own_dir.map(str::to_owned),
            key,
            workflow: self.plan.workflow,
            staging: path.to_owned(),
            location,
            now: self.now,
        }
        .admit();
        admission.current(None).map_err(unavailable)?;
        let _claim = super::super::browser_window::ProfileLock::claim(&path)
            .map_err(|_| unavailable("Kimi published profile is held or unavailable"))?;
        let deadline = std::time::Instant::now() + AUTH_CHECK_BUDGET;
        let proof = match host.kimi_native_profile(
            &path,
            region,
            expected,
            self.now,
            deadline,
            std::sync::Arc::new(Guard(admission.clone())),
        ) {
            Err(ObservationError::ReauthenticationRequired(_)) => {
                admission.current(None).map_err(unavailable)?;
                let snapshot = shared_provider::kimi_rejected_snapshot(&path, region, expected)
                    .map_err(shared_provider::kimi_observation_error)?
                    .ok_or_else(|| unavailable("Kimi rejection no longer matches its profile"))?;
                let raw = records
                    .raw_roster
                    .row(&self.row.label)
                    .ok_or_else(|| unavailable("Kimi account policy is absent"))?;
                self.store
                    .tail(accounts::LEDGER_KEEP_ROWS)
                    .map_err(|error| unavailable(error.to_string()))?;
                if super::super::accounts_sync::unaddressed_sign_in_task_at(
                    self.store,
                    &records.logins,
                    raw,
                )
                .into_iter()
                .chain(super::super::accounts_sync::bot_check_on_record_at(
                    self.store,
                    raw,
                    &records.logins,
                ))
                .any(|at| {
                    host.now().max(self.now).saturating_sub(at)
                        < super::super::accounts_sync::PERSON_BACKOFF_SECS
                }) {
                    return Err(unavailable("Kimi reauthentication retry is held"));
                }
                let home = self
                    .store
                    .user_home()
                    .ok_or_else(|| unavailable("Kimi account home is unavailable"))?;
                if super::super::headless_oauth::find_headless_browser_with(
                    Some(home),
                    &|name| host.machine_var(name),
                    host.system_browsers(),
                )
                .is_none()
                {
                    return Err(unavailable("Kimi reauthentication has no headless browser"));
                }
                if let Some(program) = host.sign_in_program(Cli::Kimi)
                    && crate::ai::facts::launch_finds_nothing(
                        &program,
                        host.launch_path().as_deref(),
                    )
                {
                    return Err(unavailable("Kimi reauthentication program is unavailable"));
                }
                admission.current(None).map_err(unavailable)?;
                snapshot.check_current().map_err(unavailable)?;
                return Ok(Some(Outcome::Reauthenticate(snapshot)));
            }
            result => result?.ok_or_else(|| {
                ObservationError::RecoveryRequired(
                    "Kimi published profile has no verifiable native credential".into(),
                )
            })?,
        };
        if !accounts::email_matches(expected, proof.identity()) {
            return Err(ObservationError::RecoveryRequired(
                "Kimi profile reported another identity".into(),
            ));
        }
        admission
            .current(Some(proof.principal()))
            .map_err(unavailable)?;
        let now = host.now().max(self.now);
        self.store
            .transact_pool_logins_within(
                &accounts::LOCK_LADDER,
                self.roster,
                now,
                &mut None,
                |raw, state, logins| {
                    self.plan.require_workflow(raw, logins, &self.row.label)?;
                    signin_policy_matches(self.row, raw, self.own_dir)?;
                    pool_admission::snapshot_current(self.row, state, logins, key)?;
                    shared_renewal::unmanaged(logins, self.row, Some(proof.principal()), true)?;
                    let current = logins
                        .get(&self.row.label)
                        .ok_or("Kimi live binding disappeared")?;
                    if current.dir != before {
                        return Err("Kimi live binding changed".into());
                    }
                    let roster = accounts::bound(raw, logins);
                    if accounts::retained_directory_owner_at(
                        logins,
                        &roster.rows,
                        &self.row.label,
                        dir,
                        self.store.default_home().as_deref(),
                    )?
                    .is_some()
                        || accounts::shared_staging_retained(logins, dir)?
                    {
                        return Err("Kimi live profile custody changed".into());
                    }
                    // This local check includes any retained native lease; it does
                    // no HTTP and invokes no Store-reading authority callback.
                    proof.check_current()?;
                    let was = current.was.clone();
                    let events = accounts::note_renewed(
                        state,
                        logins,
                        &self.row.label,
                        self.own_dir,
                        dir,
                        proof.subscription(),
                        now,
                    );
                    // An in-place token rotation supersedes no directory.
                    logins
                        .labels
                        .get_mut(&self.row.label)
                        .ok_or("Kimi live binding disappeared")?
                        .was = was;
                    accounts::note_identity(
                        logins,
                        &self.row.label,
                        accounts::Proof::Status,
                        Some(expected),
                    );
                    Ok(events)
                },
            )
            .map_err(unavailable)?;
        Ok(Some(Outcome::Renewed))
    }
}
