// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The source observation retained by an automatic account sync.

use super::*;
use crate::harness::alab_roster::{ReceiptGuard, SourceCapture};

pub(super) struct AutomaticSource {
    config: PathBuf,
    config_image: Option<String>,
    observation: SourceCapture,
    policy: Option<accounts::source_policy::Snapshot>,
    retire_legacy: bool,
    selection: Option<SelectionObservation>,
    receipt: Option<ReceiptGuard>,
}

struct LocalReceiptGuard {
    source: ReceiptGuard,
    config: PathBuf,
    image: Option<String>,
    policy: Option<accounts::source_policy::Snapshot>,
}

impl crate::harness::alab_roster::SourceGuard for LocalReceiptGuard {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn same_source(&self, other: &dyn crate::harness::alab_roster::SourceGuard) -> bool {
        other.as_any().downcast_ref::<Self>().is_some_and(|other| {
            self.source == other.source
                && self.config == other.config
                && self.image == other.image
                && self.policy == other.policy
        })
    }
    fn with_admitted(
        &self,
        save: Box<dyn FnOnce() -> Result<(), SyncError> + '_>,
    ) -> Result<(), SyncError> {
        self.source.with_admitted(|| {
            // Local reads only: re-entering global source capture here would
            // recursively acquire the native registry's admission locks.
            if read_config(&self.config).map_err(|why| SyncError::retry(Retry::Unavailable, why))?
                != self.image
            {
                return Err(SyncError::retry(
                    Retry::Changed,
                    "account configuration changed before receipt admission",
                ));
            }
            if let Some(policy) = &self.policy {
                policy
                    .check_current()
                    .map_err(|why| SyncError::retry(Retry::Changed, why))?;
            }
            save()
        })
    }
}

struct SelectionObservation {
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    roster: PathBuf,
    scope: SourceScope,
    selected: SelectedSource,
}

/// A manual metadata edit retains its source decision until receipt admission.
/// Only the local fleet is writable; neither this plan nor its guard grants a login.
pub(in crate::harness) struct ManualRosterPlan {
    pub(in crate::harness) fleet: Option<LocalFleetEdit>,
    captured: AutomaticSource,
    protection: SourceProtection,
    roster: PathBuf,
    destination: PathBuf,
}

#[cfg(test)]
thread_local! {
    pub(in crate::harness) static MANUAL_EARLY_FLEET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn manual_protection(
    cli: Cli,
    env: &Env,
    config: &Path,
    image: Option<&str>,
) -> Result<SourceProtection, String> {
    let mut protection = SourceProtection {
        home: env.home.clone(),
        ..SourceProtection::default()
    };
    if let Some(home) = &env.home {
        protection
            .read_only
            .push(home.join(".secrets").join(cli_fleet_file(cli)));
    }
    if let Some(source) = image.and_then(source_key_text)
        && !source.starts_with("http://")
        && !source.starts_with("https://")
    {
        let input = if let Some(relative) = source.strip_prefix("~/") {
            source_path(
                env.home.as_deref().ok_or("no home for account source")?,
                Path::new(relative),
            )?
        } else {
            let base = config
                .parent()
                .ok_or("account configuration has no parent")?;
            source_path(
                &std::path::absolute(base).map_err(|why| why.to_string())?,
                Path::new(&source),
            )?
        };
        protection.read_only.push(input);
    }
    Ok(protection)
}

/// Explicit native selection bypasses default source planning, never output protection.
pub(in crate::harness) fn protect_manual_roster(
    cli: Cli,
    env: &Env,
    roster: &Path,
) -> Result<(), String> {
    let config = env.config.clone().unwrap_or_else(|| config_beside(roster));
    let image = read_config(&config)?;
    let protection = manual_protection(cli, env, &config, image.as_deref())?;
    let destination = accounts::usage_target(Some(roster))?.ok_or("missing account roster")?;
    protection.check(&[roster, &destination])
}

impl ManualRosterPlan {
    #[cfg(test)]
    pub(in crate::harness) fn early_write_negative_control(&self) -> Result<(), String> {
        if MANUAL_EARLY_FLEET.get()
            && let Some(fleet) = &self.fleet
        {
            fs::write(&fleet.destination, &fleet.after).map_err(|why| why.to_string())?;
        }
        Ok(())
    }

    pub(in crate::harness) fn capture(
        cli: Cli,
        env: &Env,
        roster: &Path,
        row: &Row,
    ) -> Result<Self, String> {
        let store = env
            .accounts_store_for(cli)
            .ok_or_else(|| super::super::cli::no_home_for(cli))?;
        let config = env.config.clone().unwrap_or_else(|| config_beside(roster));
        let mut captured =
            AutomaticSource::capture_requested(&config).map_err(SyncError::into_message)?;
        let native = captured
            .chosen(cli, roster, None)
            .map_err(SyncError::into_message)?
            .is_some();
        if native {
            captured.native_guard().map_err(SyncError::into_message)?;
        }
        let policy = store.source_policy()?;
        let selected = if cli == Cli::Claude && !native {
            Some(source_selection::select_observed(
                env.home.as_deref(),
                env.config.as_deref(),
                Some(roster),
                SourceScope::Init,
                &policy,
            )?)
        } else {
            None
        };
        let retire = native
            || selected.as_ref().is_some_and(|selected| {
                selected.retires_legacy(env.home.as_deref(), env.config.as_deref())
            });
        captured = captured.with_policy(policy, retire);
        if let Some(selected) = &selected {
            captured = captured.with_selection(env, roster, SourceScope::Init, selected.clone());
        }
        let protection = manual_protection(cli, env, &config, captured.config_image.as_deref())?;
        let destination = accounts::usage_target(Some(roster))?.ok_or("missing account roster")?;
        protection.check(&[roster, &destination])?;
        let fleet = match selected.as_ref().map(SelectedSource::origin) {
            Some(FleetOrigin::Local { files, .. }) => {
                let [file] = files.as_slice() else {
                    return Err("manual account publication needs exactly one local fleet".into());
                };
                protection.check(&[file])?;
                Some(local_fleet_edit(env, file, row)?)
            }
            Some(origin @ (FleetOrigin::Key(_) | FleetOrigin::Vault(_))) => {
                let from = if matches!(&origin, FleetOrigin::Key(_)) {
                    "list the account there"
                } else {
                    "list the account where that file comes from (the vault's entry, on a Mac that edits the vault)"
                };
                return Err(format!(
                    "this machine's accounts come from {}, which a sync writes {} from and this machine does not write, so a row listed here alone would be dropped at the next sync: {from} (its email, and this machine among its `machines`) before changing the local roster; nothing was written",
                    origin.words(),
                    roster.display()
                ));
            }
            Some(FleetOrigin::AlabPending(why) | FleetOrigin::SourcePending(why)) => {
                return Err(why);
            }
            Some(FleetOrigin::Roster | FleetOrigin::Alab) | None => None,
        };
        let plan = Self {
            fleet,
            captured,
            protection,
            roster: roster.to_owned(),
            destination,
        };
        plan.check_current()?;
        Ok(plan)
    }

    pub(in crate::harness) fn retires_legacy(&self) -> bool {
        self.captured.retires_legacy()
    }

    pub(in crate::harness) fn guard(&self) -> Option<&ReceiptGuard> {
        self.captured.receipt.as_ref()
    }

    /// Recheck outside native custody: validation opens the native capture once.
    /// The retained native guard then fences the receipt save itself.
    pub(in crate::harness) fn check_current(&self) -> Result<(), String> {
        self.captured.validate().map_err(SyncError::into_message)?;
        if accounts::usage_target(Some(&self.roster))?.as_ref() != Some(&self.destination) {
            return Err("account roster destination changed after planning".into());
        }
        self.protection.check(&[&self.roster, &self.destination])?;
        if let Some(fleet) = &self.fleet {
            self.protection.check(&[&fleet.path, &fleet.destination])?;
            if accounts::usage_target(Some(&fleet.path))?.as_ref() != Some(&fleet.destination) {
                return Err("local fleet destination changed after planning".into());
            }
        }
        Ok(())
    }
}

impl AutomaticSource {
    pub(super) fn capture(config: &Path) -> Result<Self, SyncError> {
        Self::capture_inner(config, true)
    }

    pub(super) fn capture_requested(config: &Path) -> Result<Self, SyncError> {
        Self::capture_inner(config, false)
    }

    fn capture_inner(config: &Path, automatic: bool) -> Result<Self, SyncError> {
        let config_image = read_config(config)?;
        let policy = crate::supervise::config::SupervisorConfig::from_aterm_toml(
            config_image.as_deref().unwrap_or(""),
        )
        .0;
        if automatic && (!policy.enabled || policy.accounts == accounts::Mode::Off) {
            return Err(ACCOUNTS_SYNC_OFF.into());
        }
        let mut captured = Self {
            config: config.to_path_buf(),
            config_image,
            observation: crate::harness::alab_roster::capture_for(config),
            policy: None,
            retire_legacy: false,
            selection: None,
            receipt: None,
        };
        captured.retain_receipt_guard();
        captured.validate()?;
        Ok(captured)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSourceCapture",
            action = "Resolve",
            project = "aterm_agent::harness::accounts_sync::source_admission::tests::resolve"
        )
    )]
    pub(super) fn validate(&self) -> Result<(), SyncError> {
        if let Some(policy) = &self.policy {
            policy
                .check_current()
                .map_err(|why| SyncError::retry(Retry::Changed, why))?;
            if let Some(observation) = &self.selection
                && source_selection::select_observed(
                    observation.home.as_deref(),
                    observation.config.as_deref(),
                    Some(&observation.roster),
                    observation.scope,
                    policy,
                )
                .map_err(|why| SyncError::retry(Retry::Changed, why))?
                    != observation.selected
            {
                return Err(SyncError::retry(
                    Retry::Changed,
                    "automatic account source precedence changed after capture",
                ));
            }
            if let Some(observation) = &self.selection
                && observation
                    .selected
                    .retires_legacy(observation.home.as_deref(), observation.config.as_deref())
                    != self.retire_legacy
            {
                return Err(SyncError::retry(
                    Retry::Changed,
                    "automatic account source assignment changed after capture",
                ));
            }
        }
        if read_config(&self.config).map_err(|why| SyncError::retry(Retry::Unavailable, why))?
            != self.config_image
            || !self
                .observation
                .same_source(&crate::harness::alab_roster::capture_for(&self.config))
        {
            return Err(SyncError::retry(
                Retry::Changed,
                "automatic account source changed after capture; no new sync was admitted"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) fn with_policy(
        mut self,
        policy: accounts::source_policy::Snapshot,
        retire_legacy: bool,
    ) -> Self {
        self.policy = Some(policy);
        self.retire_legacy = retire_legacy;
        self.retain_receipt_guard();
        self
    }

    pub(super) fn retires_legacy(&self) -> bool {
        self.retire_legacy
    }

    pub(super) fn native_guard(&self) -> Result<&ReceiptGuard, SyncError> {
        self.receipt.as_ref().ok_or_else(|| {
            SyncError::retry(
                Retry::Unavailable,
                "native account source has no receipt admission",
            )
        })
    }

    pub(super) fn named_account(&self, cli: Cli, name: &str) -> Result<Option<Row>, String> {
        self.validate().map_err(SyncError::into_message)?;
        if self.observation.snapshot().pending() {
            return Err("the native account source is unavailable; no account was added".into());
        }
        super::super::alab_roster::asked_row_from(self.observation.snapshot(), cli, name)
            .transpose()
    }

    fn retain_receipt_guard(&mut self) {
        self.receipt = self.observation.native_guard().ok().map(|source| {
            ReceiptGuard::new(LocalReceiptGuard {
                source: source.clone(),
                config: self.config.clone(),
                image: self.config_image.clone(),
                policy: self.policy.clone(),
            })
        });
    }

    pub(super) fn owns_local_projection(&self, path: &Path) -> bool {
        self.selection.as_ref().is_some_and(|observation| {
            matches!(&observation.selected, SelectedSource::Beside { path: selected, .. } if selected == path)
        })
    }

    pub(super) fn with_selection(
        mut self,
        env: &Env,
        roster: &Path,
        scope: SourceScope,
        selected: SelectedSource,
    ) -> Self {
        self.selection = Some(SelectionObservation {
            home: env.home.clone(),
            config: env.config.clone(),
            roster: roster.to_path_buf(),
            scope,
            selected,
        });
        self
    }

    pub(super) fn chosen(
        &self,
        cli: Cli,
        roster: &Path,
        named: Option<&str>,
    ) -> Result<Option<FleetRoster>, SyncError> {
        self.validate()?;
        super::super::alab_roster::chosen_captured(
            self.observation.snapshot(),
            cli,
            Some(roster),
            named,
            self.config_image
                .as_deref()
                .and_then(super::source_key_text)
                .is_some(),
        )
        .map_err(|why| {
            if self.observation.snapshot().pending() {
                SyncError::retry(Retry::Unavailable, why)
            } else {
                why.into()
            }
        })
    }
}

fn read_config(config: &Path) -> Result<Option<String>, String> {
    let text = aterm_types::dirs::read_aterm_config_text(config)
        .map_err(|_| ACCOUNTS_SYNC_OFF.to_owned())?;
    if let Some(text) = &text {
        aterm_toml::from_str::<aterm_toml::Table>(text).map_err(|_| {
            "automatic account configuration is invalid; no new sync was admitted".to_owned()
        })?;
    }
    Ok(text)
}

/// An automatic candidate is a location, never an explicit source override.
pub(super) enum SourceRequest<'a> {
    Requested(Option<&'a str>),
    RequestedNative(&'a AutomaticSource),
    Automatic {
        candidate: Option<&'a str>,
        admission: &'a AutomaticSource,
    },
}

impl<'a> SourceRequest<'a> {
    pub(super) fn candidate(&self) -> Option<&'a str> {
        match self {
            Self::Requested(source) => *source,
            Self::RequestedNative(_) => Some(super::super::alab_roster::SOURCE),
            Self::Automatic { candidate, .. } => *candidate,
        }
    }

    pub(super) fn admission(&self) -> Option<&AutomaticSource> {
        match self {
            Self::Requested(_) => None,
            Self::RequestedNative(admission) | Self::Automatic { admission, .. } => Some(admission),
        }
    }

    pub(super) fn validate(&self) -> Result<(), SyncError> {
        self.admission().map_or(Ok(()), AutomaticSource::validate)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSourceCapture",
            action = "Resolve",
            project = "aterm_agent::harness::accounts_sync::source_admission::tests::resolve"
        )
    )]
    pub(super) fn chosen(
        &self,
        cli: Cli,
        roster: &Path,
        config: Option<&Path>,
    ) -> Result<Option<FleetRoster>, SyncError> {
        self.validate()?;
        match self {
            Self::Requested(source) => {
                super::super::alab_roster::chosen(cli, Some(roster), *source, config)
                    .map_err(Into::into)
            }
            Self::RequestedNative(admission) => {
                admission.chosen(cli, roster, Some(super::super::alab_roster::SOURCE))
            }
            Self::Automatic { admission, .. } => admission.chosen(cli, roster, None),
        }
    }
}

#[cfg(test)]
#[path = "accounts_source_admission_tests.rs"]
mod tests;
