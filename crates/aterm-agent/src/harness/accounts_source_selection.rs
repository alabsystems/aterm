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
        origin: FleetOrigin,
        own_copy: bool,
    },
    Sheet {
        path: PathBuf,
        displaced_copy: Option<PathBuf>,
    },
    Beside {
        path: PathBuf,
        opt_in: Option<OptIn>,
    },
    Absent,
}

impl Selection {
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
            Self::Pool(path) | Self::Sheet { path, .. } => FleetOrigin::Vault(path.clone()),
            Self::VaultFleet { origin, .. } => origin.clone(),
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
pub(super) fn select(
    home: Option<&Path>,
    config: Option<&Path>,
    roster: Option<&Path>,
    scope: Scope,
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
                !beside
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
                    displaced_copy: Some(vault),
                };
            }
            let local = read_local_source_text(&vault).ok().and_then(|text| {
                let fleet = FleetRoster::from_toml(&text, &vault.display().to_string()).ok()?;
                let opt_in = opt_in_of(&text);
                (own || fleet_of_this_machine(opt_in, &fleet)).then_some(opt_in)
            });
            let origin = if let Some(opt_in) = local {
                FleetOrigin::Local {
                    files: beside
                        .iter()
                        .filter(|path| source_entry_present(path))
                        .cloned()
                        .chain([vault.clone()])
                        .collect(),
                    opt_in: if named { beside_opt_in } else { opt_in },
                }
            } else {
                FleetOrigin::Vault(vault.clone())
            };
            return Selection::VaultFleet {
                path: vault,
                origin,
                own_copy: own,
            };
        }
        if let Some(path) = sheet {
            return Selection::Sheet {
                path,
                displaced_copy: None,
            };
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

#[cfg(test)]
#[path = "accounts_source_selection_tests.rs"]
mod tests;
