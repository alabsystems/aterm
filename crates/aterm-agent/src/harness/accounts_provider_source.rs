// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Captured provider inputs, shared by planning and writer admission.

use super::*;

enum Selected {
    Native,
    /// The vault entry the pool syncs from by default, with the text read when captured.
    Local(Option<LocalCliSource>),
    /// A named source: where it is a local file or a vault entry, where to read it again.
    Explicit(Option<SnapshotAt>),
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
            let at = match fetch_plan(env, named)? {
                FetchPlan::File(path) => Some(SnapshotAt::File(path)),
                FetchPlan::Entry(name) => Some(SnapshotAt::Entry(name)),
                FetchPlan::Http(_) => None,
            };
            (Selected::Explicit(at), Some(fetch(env, named)?))
        } else {
            let selected = Self::local(cli, home, &policy);
            let raw = selected
                .as_ref()
                .map(|selected| {
                    selected
                        .entry
                        .text()
                        .map_err(|why| format!("{}: {why}", selected.entry.source()))
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
    ) -> Option<LocalCliSource> {
        local_cli_source_observed(cli, home, policy)
    }

    fn source_name(&self) -> Option<String> {
        match &self.selected {
            Selected::Native => Some(super::super::alab_roster::SOURCE.to_owned()),
            Selected::Local(selected) => selected.as_ref().map(|s| s.entry.source()),
            Selected::Explicit(_) => self.named.clone(),
        }
    }

    fn is_pool(&self) -> bool {
        self.source_name()
            .zip(self.raw.as_deref())
            .is_some_and(|(name, raw)| super::super::ai_pool::Pool::looks_like_pool(&name, raw))
    }

    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
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
        // A default source is a vault entry read by name, with the text it held: the same
        // selection over a newer read is the same precedence and the same bytes.
        if let Selected::Local(selected) = &self.selected
            && Self::local(cli, home, &self.policy) != *selected
        {
            return Err(SyncError::retry(
                Retry::Changed,
                "provider source precedence changed after capture",
            ));
        }
        if let Selected::Explicit(Some(at)) = &self.selected {
            let current = match at {
                SnapshotAt::File(path) => {
                    read_local_source_text_with(path, astream_sys::Links::Follow)
                        .map(Some)
                        .map_err(|why| format!("{}: {why}", path.display()))
                }
                SnapshotAt::Entry(_) => snapshot_bytes(Some(home), at)
                    .map(|bytes| bytes.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
                    .map_err(|why| format!("{at}: {why}")),
            }
            .map_err(|why| SyncError::retry(Retry::Unavailable, why))?;
            if current.as_deref() != self.raw.as_deref() {
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

    pub(super) fn protect(&self, env: &Env, output: &Path) -> Result<(), String> {
        let mut protection = SourceProtection {
            home: env.home.clone(),
            read_only: Vec::new(),
        };
        if let Selected::Explicit(Some(SnapshotAt::File(path))) = &self.selected {
            protection.read_only.push(path.clone());
        }
        protection.check(&[output])
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
