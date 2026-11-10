// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Captured provider inputs, shared by planning and writer admission.

use super::*;

enum Selected {
    Native,
    Local(Option<LocalCliSource>),
    Explicit(Option<PathBuf>),
}

pub(super) struct ProviderSource {
    admission: Option<AutomaticSource>,
    policy: accounts::source_policy::Snapshot,
    selected: Selected,
    named: Option<String>,
    raw: Option<String>,
    pool_identity: Option<Option<super::super::ai_pool::LocalPoolIdentity>>,
    machine: Option<String>,
    machine_override: Option<String>,
    retire_legacy: bool,
}

impl ProviderSource {
    pub(super) fn receipt_guard(
        &self,
    ) -> Result<Option<&super::super::alab_roster::ReceiptGuard>, SyncError> {
        if matches!(self.selected, Selected::Native) {
            self.admission
                .as_ref()
                .ok_or("native source observation is missing")?
                .native_guard()
                .map(Some)
        } else {
            Ok(None)
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountProviderCapture",
            action = "Origin",
            reason = "The source may be native or a local provider file."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountProviderCapture",
            action = "Change",
            reason = "External writers may replace source data between writer acquisitions."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountProviderCapture",
            action = "Config",
            reason = "External writers may replace configuration after capture."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountProviderCapture",
            action = "Stage",
            reason = "The caller may release the writer to acquire sign-in custody."
        )
    )]
    pub(super) fn capture(
        cli: Cli,
        env: &Env,
        store: &accounts::Store,
        roster: &Path,
        (named, machine_override): (Option<&str>, Option<&str>),
        automatic: bool,
        fetch: &Fetch<'_>,
    ) -> Result<Self, SyncError> {
        let home = env.home.as_deref().ok_or("no home directory")?;
        let config = env.config.as_deref().ok_or("no configuration path")?;
        let named = named.map(str::trim).filter(|name| !name.is_empty());
        let policy = store.source_policy()?;
        let admission = if named.is_none() || named == Some(super::super::alab_roster::SOURCE) {
            Some(
                if automatic {
                    AutomaticSource::capture(config)?
                } else {
                    AutomaticSource::capture_requested(config)?
                }
                .with_policy(policy.clone(), false),
            )
        } else {
            None
        };
        let native = admission
            .as_ref()
            .map(|captured| captured.chosen(cli, roster, named))
            .transpose()?
            .flatten();
        let (selected, raw) = if native.is_some() {
            (Selected::Native, None)
        } else if let Some(named) = named {
            let path = match fetch_plan(env, named)? {
                FetchPlan::File(path) => Some(path),
                FetchPlan::Http(_) => None,
            };
            (Selected::Explicit(path), Some(fetch(env, named)?))
        } else {
            let selected = Self::local(cli, home, &policy)?;
            let raw = selected
                .as_ref()
                .map(|selected| {
                    read_local_source_text(&selected.path)
                        .map_err(|why| format!("{}: {why}", selected.path.display()))
                })
                .transpose()?;
            (Selected::Local(selected), raw)
        };
        let mut captured = Self {
            admission,
            policy,
            selected,
            named: named.map(str::to_owned),
            raw,
            pool_identity: None,
            machine: machine_for(Some(config), machine_override),
            machine_override: machine_override.map(str::to_owned),
            retire_legacy: false,
        };
        if captured.is_pool() {
            captured.pool_identity = Some(super::super::ai_pool::resolve_local_pool_identity(
                Some(home),
                Some(config),
                machine_override,
            )?);
        }
        // Refuse malformed input before migration or any other account mutation.
        if let Some((fleet, _, machine)) = captured.load(cli, roster)? {
            captured.retire_legacy = captured.named.is_none()
                && machine.is_some()
                && (matches!(captured.selected, Selected::Native)
                    || captured.is_pool()
                    || (matches!(&captured.selected, Selected::Local(Some(source)) if source.csv)
                        && machine.as_deref().is_some_and(|machine| {
                            !fleet.roster.accounts_for_machine(machine).is_empty()
                        })));
        }
        captured.validate(cli, env)?;
        Ok(captured)
    }

    fn local(
        cli: Cli,
        home: &Path,
        policy: &accounts::source_policy::Snapshot,
    ) -> Result<Option<LocalCliSource>, String> {
        local_cli_source_observed(cli, home, policy, &mut |path, pool| {
            if pool {
                pool_entry_present(path)
            } else {
                source_entry_present(path)
            }
        })
    }

    fn source_name(&self) -> Option<String> {
        match &self.selected {
            Selected::Native => Some(super::super::alab_roster::SOURCE.to_owned()),
            Selected::Local(selected) => selected.as_ref().map(|s| s.path.display().to_string()),
            Selected::Explicit(_) => self.named.clone(),
        }
    }

    fn is_pool(&self) -> bool {
        self.source_name()
            .zip(self.raw.as_deref())
            .is_some_and(|(name, raw)| super::super::ai_pool::Pool::looks_like_pool(&name, raw))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountProviderCapture",
            action = "Admit",
            project = "aterm_agent::harness::accounts_sync::provider_source::tests::transition"
        )
    )]
    pub(super) fn validate(&self, cli: Cli, env: &Env) -> Result<(), SyncError> {
        #[cfg(test)]
        if SKIP_REVALIDATION.get() {
            return Ok(());
        }
        if let Some(admission) = &self.admission {
            admission.validate()?;
        } else {
            self.policy
                .check_current()
                .map_err(|why| SyncError::retry(Retry::Changed, why))?;
        }
        let home = env.home.as_deref().ok_or("no home directory")?;
        if self.retire_legacy
            && let Selected::Local(Some(selected)) = &self.selected
            && accounts::source_policy::aliases(
                &selected.path,
                &home.join(".secrets").join(cli_fleet_file(cli)),
            )?
        {
            return Err("provider replacement aliases the legacy source it would retire".into());
        }
        let path = match &self.selected {
            Selected::Local(selected) => {
                if Self::local(cli, home, &self.policy)
                    .map_err(|why| SyncError::retry(Retry::Changed, why))?
                    != *selected
                {
                    return Err(SyncError::retry(
                        Retry::Changed,
                        "provider source precedence changed after capture",
                    ));
                }
                selected.as_ref().map(|s| (s.path.as_path(), false))
            }
            Selected::Explicit(path) => path.as_deref().map(|path| (path, true)),
            Selected::Native => None,
        };
        if let Some((path, explicit)) = path {
            let links = if explicit {
                astream_sys::Links::Follow
            } else {
                astream_sys::Links::Refuse
            };
            let current = read_local_source_text_with(path, links).map_err(|why| {
                SyncError::retry(Retry::Unavailable, format!("{}: {why}", path.display()))
            })?;
            if Some(current.as_str()) != self.raw.as_deref() {
                return Err(SyncError::retry(
                    Retry::Changed,
                    "provider source bytes changed after capture",
                ));
            }
        }
        if let Some(identity) = &self.pool_identity {
            if &super::super::ai_pool::resolve_local_pool_identity(
                Some(home),
                env.config.as_deref(),
                self.machine_override.as_deref(),
            )? != identity
            {
                return Err(SyncError::retry(
                    Retry::Changed,
                    "provider machine identity changed after capture",
                ));
            }
        } else if !matches!(self.selected, Selected::Native)
            && machine_for(env.config.as_deref(), self.machine_override.as_deref()) != self.machine
        {
            return Err(SyncError::retry(
                Retry::Changed,
                "provider machine identity changed after capture",
            ));
        }
        if matches!(self.selected, Selected::Explicit(_))
            && !self.is_pool()
            && let Some((name, raw)) = self.source_name().zip(self.raw.as_deref())
        {
            // Explicit sheets still obey the current sheet-read policy. Unrelated
            // automatic source/configuration changes do not cancel a named import.
            parse_cli_source(cli, &name, raw)?;
        }
        Ok(())
    }

    pub(super) fn protect(&self, cli: Cli, env: &Env, output: &Path) -> Result<(), String> {
        let mut protection = SourceProtection {
            home: env.home.clone(),
            read_only: env
                .home
                .iter()
                .map(|home| home.join(".secrets").join(cli_fleet_file(cli)))
                .collect(),
        };
        match &self.selected {
            Selected::Local(Some(selected)) => protection.read_only.push(selected.path.clone()),
            Selected::Explicit(Some(path)) => protection.read_only.push(path.clone()),
            _ => {}
        }
        protection.check(&[output])
    }

    pub(super) fn migrate(
        &mut self,
        cli: Cli,
        env: &Env,
        store: &accounts::Store,
        held: &accounts::Lock,
        roster: &Path,
    ) -> Result<bool, SyncError> {
        if matches!(self.selected, Selected::Native)
            && let Some(lab) = self
                .admission
                .as_ref()
                .ok_or("native source observation is missing")?
                .chosen(cli, roster, self.named.as_deref())?
                .and_then(|fleet| fleet.lab_snapshot)
        {
            let migrated = super::super::alab_roster::migrate_held(
                &lab,
                cli,
                store,
                held,
                roster,
                env.now,
                (self.named.is_none(), self.receipt_guard()?),
            )?;
            if !migrated.moved.is_empty() {
                // This writer just committed its own monotonic retirement with migration.
                // Keep the original source/configuration observation and advance only policy.
                self.policy = store.source_policy()?;
                self.admission = self
                    .admission
                    .take()
                    .map(|admission| admission.with_policy(self.policy.clone(), false));
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn load(&self, cli: Cli, roster: &Path) -> Result<Option<LoadedFleet>, SyncError> {
        if matches!(self.selected, Selected::Native) {
            let fleet = self
                .admission
                .as_ref()
                .ok_or("native source observation is missing")?
                .chosen(cli, roster, self.named.as_deref())?
                .ok_or_else(|| {
                    SyncError::retry(
                        Retry::Changed,
                        "native provider source changed after capture",
                    )
                })?;
            let machine = fleet.alab.as_ref().map(|me| me.key.as_str().to_owned());
            return Ok(Some((
                CliFleet {
                    cli,
                    roster: fleet,
                    held: Vec::new(),
                },
                None,
                machine,
            )));
        }
        let Some((chosen, raw)) = self.source_name().zip(self.raw.as_deref()) else {
            return Ok(None);
        };
        if matches!(&self.selected, Selected::Local(Some(source)) if source.csv)
            && !csv_names_column(cli, raw)
        {
            return Ok(None);
        }
        if self.is_pool() {
            let pool = super::super::ai_pool::Pool::from_toml(raw, &chosen)?;
            let identity = self.pool_identity.clone().flatten();
            let machine = identity.as_ref().map(|id| id.primary.clone());
            let roster = project_pool_fleet(&pool, cli, identity.as_ref());
            return Ok(Some((
                CliFleet {
                    cli,
                    roster,
                    held: Vec::new(),
                },
                identity.map(|identity| PoolSyncCtx { pool, identity }),
                machine,
            )));
        }
        Ok(Some((
            parse_cli_source(cli, &chosen, raw)?,
            None,
            self.machine.clone(),
        )))
    }

    pub(super) fn needs_retirement(&self) -> bool {
        self.retire_legacy && !self.policy.retired()
    }

    pub(super) fn pool_text(&self) -> Option<&str> {
        self.is_pool().then_some(self.raw.as_deref()).flatten()
    }
}

#[cfg(test)]
thread_local! {
    static SKIP_REVALIDATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
#[path = "accounts_provider_source_tests.rs"]
mod tests;
