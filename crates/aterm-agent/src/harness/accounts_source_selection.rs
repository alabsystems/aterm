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
    /// The vault's pool, as this Mac's store read it when it was selected.
    Pool(VaultSource),
    /// The vault's fleet roster.
    VaultFleet {
        entry: VaultSource,
    },
    /// The vault's accounting sheet.
    Sheet {
        entry: VaultSource,
    },
    Beside {
        path: PathBuf,
        opt_in: Option<OptIn>,
    },
    Absent,
}

impl Selection {
    /// Local discovery retires a slot only when it replaces an existing input.
    /// A pool or explicit source establishes authority even before that input arrives, and a
    /// sheet that gives this machine rows replaces the vault's fleet as its list.
    pub(super) fn retires_legacy(&self, config: Option<&Path>) -> bool {
        match self {
            Self::Pool(_) | Self::Key(_) => true,
            Self::Sheet { entry } => sheet_names_this_machine(config, entry),
            Self::VaultFleet { .. } | Self::Beside { .. } | Self::Absent => false,
        }
    }

    pub(super) fn source(&self) -> Option<String> {
        match self {
            Self::Key(source) => Some(source.clone()),
            Self::Pool(entry) | Self::VaultFleet { entry } | Self::Sheet { entry } => {
                Some(entry.source())
            }
            Self::Beside { path, .. } => Some(path.to_string_lossy().into_owned()),
            Self::Absent => None,
        }
    }

    pub(super) fn origin(&self) -> FleetOrigin {
        match self {
            Self::Key(source) => FleetOrigin::Key(source.clone()),
            Self::Pool(entry) | Self::Sheet { entry } | Self::VaultFleet { entry } => {
                FleetOrigin::Vault(entry.name.clone())
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
/// key. Explicit requests are resolved by the caller, before this automatic policy. The vault's
/// pool, fleet and sheet are read from one version of the vault ([`vault_sources`]), and an
/// entry this Mac cannot read holds its place: it never lets a lower source stand in for it.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "AccountSourceSelection",
        action = "Select",
        project = "aterm_agent::harness::accounts_sync::source_selection::tests::resolve"
    )
)]
fn select_inner(
    sources: impl FnOnce() -> [Option<VaultSource>; 3],
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
    let [pool, fleet, sheet] = sources();
    if key.is_none()
        && let Some(pool) = pool.filter(VaultSource::occupies)
    {
        return Selection::Pool(pool);
    }
    if scope == Scope::Init {
        if !retired && let Some(entry) = fleet.filter(VaultSource::occupies) {
            return Selection::VaultFleet { entry };
        }
        if let Some(entry) = sheet.filter(|sheet| {
            sheet_read_here()
                && sheet.occupies()
                && (!named || sheet_names_this_machine(config, sheet))
        }) {
            return Selection::Sheet { entry };
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

/// The vault's pool, fleet roster and accounting sheet under `home`, read from one version of
/// the vault ([`crate::vault_entry::read_set`]); none without a home.
pub(super) fn vault_sources(home: Option<&Path>) -> [Option<VaultSource>; 3] {
    let names = [
        super::super::ai_pool::AI_POOL_FILE,
        FLEET_ROSTER_FILE,
        ACCOUNTS_CSV_FILE,
    ];
    let Some(home) = home else {
        return [None, None, None];
    };
    let mut reads = crate::vault_entry::read_set(home, &names, SOURCE_TEXT_MAX_BYTES).into_iter();
    names.map(|name| {
        Some(VaultSource {
            name: name.to_owned(),
            read: reads.next()?,
        })
    })
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

/// [`select_inner`] under the retained source `policy`: a retired vault fleet is never
/// selected, which no path can alias, since the vault's entries are read only by name.
pub(super) fn select_observed(
    home: Option<&Path>,
    config: Option<&Path>,
    roster: Option<&Path>,
    scope: Scope,
    policy: &accounts::source_policy::Snapshot,
) -> Result<Selection, String> {
    let selected = select_inner(
        || vault_sources(home),
        config,
        roster,
        scope,
        policy.retired(),
    );
    if let Selection::Key(source) = &selected
        && let Some(home) = home
    {
        refuse_backup_source(home, config, source)?;
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
    select_inner(|| vault_sources(home), config, roster, scope, false)
}

#[cfg(test)]
#[path = "accounts_source_selection_tests.rs"]
mod tests;
