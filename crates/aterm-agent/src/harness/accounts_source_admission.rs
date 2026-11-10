// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The source observation retained by an automatic account sync.

use super::*;
use crate::harness::alab_roster::SourceSnapshot;
use std::sync::Arc;

pub(super) struct AutomaticSource {
    config: PathBuf,
    config_image: Option<String>,
    observation: SourceSnapshot,
    policy: Option<accounts::source_policy::Snapshot>,
    retire_legacy: bool,
    selection: Option<SelectionObservation>,
}

struct SelectionObservation {
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    roster: PathBuf,
    scope: SourceScope,
    selected: SelectedSource,
}

impl AutomaticSource {
    pub(super) fn capture(config: &Path) -> Result<Self, String> {
        Self::capture_inner(config, true)
    }

    pub(super) fn capture_requested(config: &Path) -> Result<Self, String> {
        Self::capture_inner(config, false)
    }

    fn capture_inner(config: &Path, automatic: bool) -> Result<Self, String> {
        let config_image = read_config(config)?;
        let policy = crate::supervise::config::SupervisorConfig::from_aterm_toml(
            config_image.as_deref().unwrap_or(""),
        )
        .0;
        if automatic && (!policy.enabled || policy.accounts == accounts::Mode::Off) {
            return Err(ACCOUNTS_SYNC_OFF.to_owned());
        }
        let captured = Self {
            config: config.to_path_buf(),
            config_image,
            observation: crate::harness::alab_roster::snapshot(),
            policy: None,
            retire_legacy: false,
            selection: None,
        };
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
    pub(super) fn validate(&self) -> Result<(), String> {
        if let Some(policy) = &self.policy {
            policy.check_current()?;
            if let Some(observation) = &self.selection
                && source_selection::select_observed(
                    observation.home.as_deref(),
                    observation.config.as_deref(),
                    Some(&observation.roster),
                    observation.scope,
                    policy,
                )? != observation.selected
            {
                return Err("automatic account source precedence changed after capture".into());
            }
            if let Some(observation) = &self.selection
                && observation
                    .selected
                    .retires_legacy(observation.home.as_deref(), observation.config.as_deref())
                    != self.retire_legacy
            {
                return Err("automatic account source assignment changed after capture".into());
            }
        }
        if read_config(&self.config)? != self.config_image
            || !same_observation(&self.observation, &crate::harness::alab_roster::snapshot())
        {
            return Err(
                "automatic account source changed after capture; no new sync was admitted"
                    .to_owned(),
            );
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
        self
    }

    pub(super) fn retires_legacy(&self) -> bool {
        self.retire_legacy
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

fn same_observation(a: &SourceSnapshot, b: &SourceSnapshot) -> bool {
    match (a, b) {
        (SourceSnapshot::Present(a), SourceSnapshot::Present(b)) => Arc::ptr_eq(a, b) || a == b,
        (SourceSnapshot::Absent, SourceSnapshot::Absent)
        | (SourceSnapshot::Disabled, SourceSnapshot::Disabled)
        | (SourceSnapshot::Pending, SourceSnapshot::Pending) => true,
        _ => false,
    }
}

/// An automatic candidate is a location, never an explicit source override.
pub(super) enum SourceRequest<'a> {
    Requested(Option<&'a str>),
    Automatic {
        candidate: Option<&'a str>,
        admission: &'a AutomaticSource,
    },
}

impl<'a> SourceRequest<'a> {
    pub(super) fn candidate(&self) -> Option<&'a str> {
        match self {
            Self::Requested(source) => *source,
            Self::Automatic { candidate, .. } => *candidate,
        }
    }

    pub(super) fn admission(&self) -> Option<&AutomaticSource> {
        match self {
            Self::Requested(_) => None,
            Self::Automatic { admission, .. } => Some(admission),
        }
    }

    pub(super) fn validate(&self) -> Result<(), String> {
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
    ) -> Result<Option<FleetRoster>, String> {
        self.validate()?;
        match self {
            Self::Requested(source) => {
                super::super::alab_roster::chosen(cli, Some(roster), *source, config)
            }
            Self::Automatic { admission, .. } => super::super::alab_roster::chosen_from(
                &admission.observation,
                cli,
                Some(roster),
                None,
                config,
            ),
        }
    }
}

#[cfg(test)]
#[path = "accounts_source_admission_tests.rs"]
mod tests;
