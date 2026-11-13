// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unattended adoption consent for the ALab toolset.

use crate::config::PackagesConfig;
use crate::store::Layout;

/// What [`adoption`] answers: consent, or the first reason there is none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adoption {
    /// Every check passed: the unattended lanes may adopt the toolset here.
    Consented,
    /// `uninstall --all` recorded a decline on this store.
    Declined,
    /// A `[packages] prefix` this build ignores is still in aterm.toml.
    IgnoredPrefix,
    /// `[packages].auto_install = false`, or a table too broken to read a consent from.
    AutoInstallOff,
    /// This build pins no root key, so the manager is inert.
    ManagerDisabled,
}

/// Whether the unattended lanes may adopt the ALab toolset on `layout`, or the refusal reason.
#[must_use]
pub fn adoption(layout: &Layout, cfg: &PackagesConfig) -> Adoption {
    adoption_with(layout.declined().is_file(), cfg, crate::manager_enabled())
}

/// Returns true when [`adoption`] consents.
#[must_use]
pub fn adoption_consented(layout: &Layout, cfg: &PackagesConfig) -> bool {
    adoption(layout, cfg) == Adoption::Consented
}

/// [`UPDATES_SWITCH`]'s words, spelled once for it and the offs that `concat!` them: a macro,
/// because `concat!` takes literals, not consts.
macro_rules! updates_switch {
    () => {
        "Keep ALab tools up to date"
    };
}

/// [`INSTALLS_SWITCH`]'s words, as a literal for `concat!`.
macro_rules! installs_switch {
    () => {
        "Install ALab tools"
    };
}

/// The Settings ▸ Packages switch that writes `[packages] enabled` ([`PackagesConfig::enabled`]).
pub const UPDATES_SWITCH: &str = updates_switch!();

/// The Settings ▸ Packages switch that writes `[packages] auto_install`
/// ([`PackagesConfig::auto_install`]).
pub const INSTALLS_SWITCH: &str = installs_switch!();

/// The words [`unattended_install_off`] gives where `[packages] enabled = false` switches
/// [`UPDATES_SWITCH`] off.
pub const UPDATES_OFF: &str = concat!(
    "Settings ▸ Packages ▸ \"",
    updates_switch!(),
    "\" is off (`[packages] enabled = false` in aterm.toml)"
);

/// The words [`unattended_install_off`] gives where the retired `[packages] auto_update = false`
/// keeps [`UPDATES_SWITCH`] off ([`PackagesConfig::enabled`] still honours it).
pub const RETIRED_UPDATES_OFF: &str = concat!(
    "Settings ▸ Packages ▸ \"",
    updates_switch!(),
    "\" is off (the retired `[packages] auto_update = false` in aterm.toml)"
);

/// THE PERSON'S NO TO UNATTENDED INSTALLS: why atpkg installs nothing nobody named on `layout`
/// now, in a person's words, or `None` where it may. It is [`adoption`]'s consent (a recorded
/// decline, an ignored `[packages] prefix`, [`INSTALLS_SWITCH`] off by `[packages].auto_install =
/// false` or the retired `seed_install = false`, or a table too broken to read one, an inert
/// manager) with [`UPDATES_SWITCH`] on ([`PackagesConfig::enabled`]: `[packages].enabled`, or the
/// retired `auto_update`), and a person's off is named before an inert manager. The update pass
/// completes the default set only where this is `None`, and a worker's spawn installs its absent
/// agent CLI only where it is, so a person who said no to installs gets no download from either.
#[must_use]
pub fn unattended_install_off(layout: &Layout, cfg: &PackagesConfig) -> Option<&'static str> {
    unattended_off_with(layout.declined().is_file(), cfg, crate::manager_enabled())
}

/// The pure core of [`unattended_install_off`], over [`adoption_with`]'s inputs.
#[must_use]
fn unattended_off_with(
    declined: bool,
    cfg: &PackagesConfig,
    manager_enabled: bool,
) -> Option<&'static str> {
    match adoption_with(declined, cfg, manager_enabled) {
        Adoption::Consented | Adoption::ManagerDisabled if !cfg.enabled() => {
            Some(if cfg.enabled == Some(false) {
                UPDATES_OFF
            } else {
                RETIRED_UPDATES_OFF
            })
        }
        Adoption::Consented => None,
        Adoption::Declined => Some(
            "a person removed the ALab toolset here (`aterm pkg uninstall --all` recorded a \
             decline; `aterm pkg install --default-set` lifts it)",
        ),
        Adoption::IgnoredPrefix => {
            Some("aterm.toml names a `[packages] prefix` this build ignores")
        }
        Adoption::AutoInstallOff if cfg.unreadable => {
            Some("aterm.toml's `[packages]` table cannot be read, so it gives no install consent")
        }
        Adoption::AutoInstallOff if cfg.auto_install.is_none() => Some(concat!(
            "Settings ▸ Packages ▸ \"",
            installs_switch!(),
            "\" is off (the retired `[packages] seed_install = false` in aterm.toml)"
        )),
        Adoption::AutoInstallOff => Some(concat!(
            "Settings ▸ Packages ▸ \"",
            installs_switch!(),
            "\" is off (`[packages] auto_install = false` in aterm.toml)"
        )),
        Adoption::ManagerDisabled => {
            Some("the package manager is inert (no package root key is pinned in this build)")
        }
    }
}

/// Whether `layout` has no active non-vendor ALab program and no recorded success yet.
#[must_use]
pub fn first_install_ahead(layout: &Layout) -> bool {
    holds_no_alab_program(layout)
        && crate::status::read(layout).is_none_or(|status| status.last_success_at.is_empty())
}

/// Whether `layout` has no active non-vendor ALab program.
#[must_use]
pub fn holds_no_alab_program(layout: &Layout) -> bool {
    !crate::active_builds(layout)
        .keys()
        .any(|program| !crate::vendor_direct::is_vendor(program))
}

/// Returns the managed `bin/` directory for a session's PATH when `home` holds the shell hook or
/// the automatic lanes are enabled on a consenting machine.
#[must_use]
pub fn session_bin_dir(
    layout: &Layout,
    cfg: &PackagesConfig,
    home: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let surfaced = (cfg.enabled() && adoption_consented(layout, cfg))
        || home.is_some_and(crate::hooks::written);
    surfaced.then(|| layout.bin_dir())
}

/// The pure core of [`adoption`]: the decline marker's presence and whether the manager is
/// armed are inputs, so every order can be driven without a store or a build that pins no
/// key.
#[must_use]
fn adoption_with(declined: bool, cfg: &PackagesConfig, manager_enabled: bool) -> Adoption {
    if declined {
        Adoption::Declined
    } else if cfg.ignored_prefix.is_some() {
        Adoption::IgnoredPrefix
    } else if !cfg.installs_unattended() {
        Adoption::AutoInstallOff
    } else if !manager_enabled {
        Adoption::ManagerDisabled
    } else {
        Adoption::Consented
    }
}

#[cfg(test)]
#[path = "adoption_tests.rs"]
mod tests;
