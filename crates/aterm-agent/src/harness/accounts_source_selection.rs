// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local source precedence shared by account readers. This chooses an input, not authority
//! to commit it: the caller still captures and validates its policy and bytes.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    /// A source-less manual sync reads an opted-in source, without starting discovery.
    Configured,
    /// Init also considers the vault files and an unannotated beside file.
    Init,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Selection {
    Key(String),
    Pool(PathBuf),
    VaultFleet {
        path: PathBuf,
    },
    Sheet {
        path: PathBuf,
    },
    Beside {
        path: PathBuf,
        opt_in: Option<OptIn>,
    },
    Absent,
}

impl Selection {
    /// Local discovery retires a slot only when it replaces an existing input.
    /// A pool or explicit source establishes authority even before that input arrives.
    pub(super) fn retires_legacy(&self, home: Option<&Path>, config: Option<&Path>) -> bool {
        match self {
            Self::Pool(_) | Self::Key(_) => true,
            Self::Sheet { path } => sheet_names_this_machine(config, path),
            Self::Beside { path, opt_in } => {
                opt_in.is_some_and(OptIn::opts_in_by_file)
                    && home.is_some_and(|home| {
                        let legacy = home.join(".secrets").join(FLEET_ROSTER_FILE);
                        own_secrets_copy(home, &legacy, path)
                            || superseded_secrets_copy(home, &legacy, path)
                    })
            }
            Self::VaultFleet { .. } | Self::Absent => false,
        }
    }

    pub(super) fn source(&self) -> Option<String> {
        match self {
            Self::Key(source) => Some(source.clone()),
            Self::Pool(path)
            | Self::VaultFleet { path, .. }
            | Self::Sheet { path, .. }
            | Self::Beside { path, .. } => Some(path.to_string_lossy().into_owned()),
            Self::Absent => None,
        }
    }

    pub(super) fn origin(&self) -> FleetOrigin {
        match self {
            Self::Key(source) => FleetOrigin::Key(source.clone()),
            Self::Pool(path) | Self::Sheet { path, .. } | Self::VaultFleet { path } => {
                FleetOrigin::Vault(path.clone())
            }
            Self::Beside { path, opt_in } => FleetOrigin::Local {
                files: vec![path.clone()],
                opt_in: *opt_in,
            },
            Self::Absent => FleetOrigin::Roster,
        }
    }

    pub(super) fn opt_in(&self) -> OptIn {
        match self {
            Self::Key(_) => OptIn::Key,
            _ => OptIn::Named,
        }
    }
}

/// A present but unusable selected entry stays selected, so its read reports the failure.
/// A configured key disables the default pool even when a named beside file outranks the
/// key. Explicit requests are resolved by the caller, before this automatic policy.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSourceSelection",
        action = "Select",
        project = "aterm_agent::harness::accounts_sync::source_selection::tests::resolve"
    )
)]
fn select_inner(
    home: Option<&Path>,
    config: Option<&Path>,
    roster: Option<&Path>,
    scope: Scope,
    retired: bool,
) -> Selection {
    let beside = roster.and_then(fleet_file_beside);
    let beside_opt_in = beside.as_deref().and_then(fleet_file_opt_in);
    let named = beside_opt_in.is_some_and(OptIn::opts_in_by_file);
    let key = config.and_then(source_key);
    if !named && let Some(key) = &key {
        return Selection::Key(key.clone());
    }
    let secrets = home.map(|home| home.join(".secrets"));
    if key.is_none()
        && let Some(pool) = secrets
            .as_ref()
            .map(|dir| dir.join(super::super::ai_pool::AI_POOL_FILE))
            .filter(|path| pool_entry_present(path))
    {
        return Selection::Pool(pool);
    }
    if scope == Scope::Init {
        let sheet = secrets
            .as_ref()
            .map(|dir| dir.join(ACCOUNTS_CSV_FILE))
            .filter(|path| sheet_read_here() && source_entry_present(path));
        if let Some((home, vault)) = home
            .zip(
                secrets
                    .as_ref()
                    .map(|dir| dir.join(FLEET_ROSTER_FILE))
                    .filter(|path| source_entry_present(path)),
            )
            .filter(|(home, vault)| {
                !retired
                    && !beside
                        .as_deref()
                        .is_some_and(|beside| superseded_secrets_copy(home, vault, beside))
            })
        {
            let own = beside
                .as_deref()
                .is_some_and(|beside| own_secrets_copy(home, &vault, beside));
            if own
                && let Some(sheet) = &sheet
                && sheet_names_this_machine(config, sheet)
            {
                return Selection::Sheet {
                    path: sheet.clone(),
                };
            }
            if own
                && named
                && let Some(path) = &beside
            {
                // Retire an equal legacy copy before discovery extends the
                // projection, independently of filesystem timestamp precision.
                return Selection::Beside {
                    path: path.clone(),
                    opt_in: beside_opt_in,
                };
            }
            return Selection::VaultFleet { path: vault };
        }
        if let Some(path) = sheet.filter(|path| !named || sheet_names_this_machine(config, path)) {
            return Selection::Sheet { path };
        }
    }
    if let Some(path) =
        beside.filter(|path| named || (scope == Scope::Init && source_entry_present(path)))
    {
        return Selection::Beside {
            path,
            opt_in: beside_opt_in,
        };
    }
    Selection::Absent
}

pub(super) fn select_for(
    store: &accounts::Store,
    home: Option<&Path>,
    config: Option<&Path>,
    roster: Option<&Path>,
    scope: Scope,
) -> Result<Selection, String> {
    select_observed(home, config, roster, scope, &store.source_policy()?)
}

pub(super) fn select_observed(
    home: Option<&Path>,
    config: Option<&Path>,
    roster: Option<&Path>,
    scope: Scope,
    policy: &accounts::source_policy::Snapshot,
) -> Result<Selection, String> {
    let selected = select_inner(home, config, roster, scope, policy.retired());
    if (policy.retired() || selected.retires_legacy(home, config))
        && let Some(home) = home
    {
        let path = match &selected {
            Selection::Pool(path)
            | Selection::VaultFleet { path, .. }
            | Selection::Sheet { path }
            | Selection::Beside { path, .. } => Some(path.clone()),
            Selection::Key(source)
                if !source.starts_with("http://") && !source.starts_with("https://") =>
            {
                let path = if let Some(relative) = source.strip_prefix("~/") {
                    source_path(home, Path::new(relative))?
                } else {
                    let base = config.and_then(Path::parent).unwrap_or(home);
                    let base = std::path::absolute(base).map_err(|why| why.to_string())?;
                    source_path(&base, Path::new(source))?
                };
                Some(path)
            }
            _ => None,
        };
        if let Some(path) = path
            && accounts::source_policy::aliases(
                &path,
                &home.join(".secrets").join(FLEET_ROSTER_FILE),
            )?
        {
            return Err("retired legacy source cannot be selected through another path".into());
        }
    }
    Ok(selected)
}

#[cfg(test)]
pub(super) fn select(
    home: Option<&Path>,
    config: Option<&Path>,
    roster: Option<&Path>,
    scope: Scope,
) -> Selection {
    select_inner(home, config, roster, scope, false)
}

#[cfg(test)]
#[path = "accounts_source_selection_tests.rs"]
mod tests;
