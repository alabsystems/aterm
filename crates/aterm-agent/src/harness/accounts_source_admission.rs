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

    pub(super) fn legacy_retired(&self) -> bool {
        self.policy
            .as_ref()
            .is_some_and(accounts::source_policy::Snapshot::retired)
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
