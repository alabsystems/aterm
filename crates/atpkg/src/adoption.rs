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
mod tests {
    use super::*;

    #[test]
    fn each_refusal_holds_alone_and_the_first_one_answers() {
        let consenting = PackagesConfig::default();
        let off = PackagesConfig {
            auto_install: Some(false),
            ..PackagesConfig::default()
        };
        let ignored = PackagesConfig {
            ignored_prefix: Some(std::path::PathBuf::from("/elsewhere")),
            ..PackagesConfig::default()
        };
        let ignored_and_off = PackagesConfig {
            auto_install: Some(false),
            ignored_prefix: Some(std::path::PathBuf::from("/elsewhere")),
            ..PackagesConfig::default()
        };
        let unreadable = PackagesConfig {
            unreadable: true,
            ..PackagesConfig::default()
        };
        let rows: [(bool, &PackagesConfig, bool, Adoption); 9] = [
            (false, &consenting, true, Adoption::Consented),
            (true, &consenting, true, Adoption::Declined),
            (false, &ignored, true, Adoption::IgnoredPrefix),
            (false, &off, true, Adoption::AutoInstallOff),
            (false, &unreadable, true, Adoption::AutoInstallOff),
            (false, &consenting, false, Adoption::ManagerDisabled),
            (true, &ignored_and_off, false, Adoption::Declined),
            (false, &ignored_and_off, false, Adoption::IgnoredPrefix),
            (false, &off, false, Adoption::AutoInstallOff),
        ];
        for (declined, cfg, enabled, want) in rows {
            assert_eq!(
                adoption_with(declined, cfg, enabled),
                want,
                "declined={declined} enabled={enabled} cfg={cfg:?}"
            );
        }
    }

    #[test]
    fn the_decline_marker_on_the_store_refuses() {
        let l = crate::testdir::layout("atpkg-adoption-");
        let cfg = PackagesConfig::default();
        assert_eq!(
            adoption_consented(&l, &cfg),
            crate::manager_enabled(),
            "an empty store under the default config consents wherever the build is armed"
        );
        std::fs::create_dir_all(&l.prefix).unwrap();
        std::fs::write(l.declined(), "# declined\n").unwrap();
        assert_eq!(adoption(&l, &cfg), Adoption::Declined);
        assert!(!adoption_consented(&l, &cfg));
    }

    #[test]
    fn a_session_gets_bin_where_consent_or_the_hook_already_puts_it() {
        let l = crate::testdir::layout("atpkg-session-bin-");
        let home_dir = crate::testdir::dir("atpkg-session-bin-home-");
        let home: &std::path::Path = &home_dir;
        let consenting = PackagesConfig::default();
        let off = PackagesConfig {
            auto_install: Some(false),
            ..PackagesConfig::default()
        };
        let lanes_off = PackagesConfig {
            enabled: Some(false),
            ..PackagesConfig::default()
        };
        let bin = Some(l.bin_dir());
        let armed = crate::manager_enabled();
        assert_eq!(
            session_bin_dir(&l, &consenting, Some(home)),
            bin.clone().filter(|_| armed),
            "consent with the lanes on (where the build is armed)"
        );
        assert_eq!(
            session_bin_dir(&l, &consenting, None),
            bin.clone().filter(|_| armed)
        );
        assert_eq!(
            session_bin_dir(&l, &off, Some(home)),
            None,
            "auto_install = false"
        );
        assert_eq!(
            session_bin_dir(&l, &lanes_off, Some(home)),
            None,
            "lanes off"
        );
        std::fs::create_dir_all(&l.prefix).unwrap();
        std::fs::write(l.declined(), "# declined\n").unwrap();
        assert_eq!(
            session_bin_dir(&l, &consenting, Some(home)),
            None,
            "declined"
        );
        let shell_d = home.join(".aterm").join("shell.d");
        std::fs::create_dir_all(&shell_d).unwrap();
        std::fs::write(shell_d.join("00-atpkg.zsh"), "# hook\n").unwrap();
        for cfg in [&consenting, &off, &lanes_off] {
            assert_eq!(session_bin_dir(&l, cfg, Some(home)), bin, "{cfg:?}");
        }
        assert_eq!(
            session_bin_dir(&l, &off, None),
            None,
            "no home, no hook to read"
        );
    }
}
