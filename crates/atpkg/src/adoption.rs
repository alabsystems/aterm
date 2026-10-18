// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Whether this machine consents to the UNATTENDED adoption of the ALab toolset: the one
//! predicate the first-launch seed (`cli::cmd_seed`) asks before it writes anything, and
//! that the window asks before it acts on that seed's behalf, so the two cannot drift.
//!
//! The order is the seed's, and each refusal names why:
//!
//! 1. a recorded decline (`uninstall --all`, [`crate::store::Layout::declined`]) — the
//!    lane adopts the toolset, so without this it would undo that on the next launch;
//! 2. a `[packages] prefix` this build ignores ([`crate::config::PackagesConfig::ignored_prefix`])
//!    — the toolset lives where it points, and the lane would lay a second one down;
//! 3. `[packages].auto_install = false` (or an unreadable table,
//!    [`crate::config::PackagesConfig::auto_install`]) — the ONE install consent;
//! 4. a build that pins no root key ([`crate::manager_enabled`]) — stubs laid now would
//!    promise a pass that answers "disabled".
//!
//! Only [`Adoption::Consented`] lets an unattended lane adopt, lay a stub or a shell hook,
//! or re-assert the rustup seam.

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

/// Whether the unattended lanes may adopt the ALab toolset on `layout` under `cfg`, and if
/// not, the first reason (module doc).
#[must_use]
pub fn adoption(layout: &Layout, cfg: &PackagesConfig) -> Adoption {
    adoption_with(layout.declined().is_file(), cfg, crate::manager_enabled())
}

/// [`adoption`] answering yes or no.
#[must_use]
pub fn adoption_consented(layout: &Layout, cfg: &PackagesConfig) -> bool {
    adoption(layout, cfg) == Adoption::Consented
}

/// Whether `layout`'s first install of the ALab toolset is still ahead of it: no ALab program
/// is active (an agent program lands on its own lane and does not count) and no pass has
/// recorded a success — a store a pass already served that still holds no ALab program (an
/// architecture no build covers, programs removed on purpose) has nothing coming. With
/// [`adoption_consented`], the window's update is owed at once: the first-run row it holds
/// ahead of that update (`aterm-gui`'s `first_run_ahead`, which asks this) and the promise its
/// seed makes the pending stubs ([`crate::progress::promise_pass`]) both rest on it.
#[must_use]
pub fn first_install_ahead(layout: &Layout) -> bool {
    holds_no_alab_program(layout)
        && crate::status::read(layout).is_none_or(|status| status.last_success_at.is_empty())
}

/// Whether `layout` holds no ALab program: none is active but the agent programs, which land
/// on their own lane ([`crate::vendor_direct::is_vendor`]) — [`first_install_ahead`]'s first
/// half, and the window's whole first-run test for the toolchain bar it shows (`aterm-gui`'s
/// `pass_is_first_run`).
#[must_use]
pub fn holds_no_alab_program(layout: &Layout) -> bool {
    !crate::active_builds(layout)
        .keys()
        .any(|program| !crate::vendor_direct::is_vendor(program))
}

/// The managed `bin/` a session's PATH carries from its first byte, APPENDED (the hook's own
/// placement, [`crate::hooks`]: a managed tool never shadows a system one), or `None`.
///
/// A launch builds its sessions' PATH before its first-launch seed writes the shell hook, and
/// a process started before that hook (an agent, a script, a shell without aterm's
/// integration) never sources it; with `bin/` already on its PATH it runs a pending stub at
/// once and the real shim the moment it lands. It is handed only where nothing new is
/// surfaced by it: `home` already holds the hook ([`crate::hooks::written`]), which appends
/// the same entry in every interactive shell, or the automatic lanes are on
/// ([`PackagesConfig::enabled`]) on a machine that consents to the adoption
/// ([`adoption_consented`]), whose launch seed writes that hook a moment later. A PATH entry
/// installs nothing, but on a declined or `auto_install = false` machine with no hook it
/// would put tools in front of a person who never asked to see them. The directory need not
/// exist: a PATH entry naming nothing is inert.
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

    /// Every input alone, and the order between them: a decline outranks an ignored
    /// prefix, which outranks `auto_install = false`, which outranks a disabled manager —
    /// the order the seed's lines are printed in.
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

    /// The store's decline marker is what [`adoption`] reads, and [`adoption_consented`]
    /// is its yes.
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

    /// A session's PATH gets `bin/` on a consenting machine whose automatic lanes run, and
    /// wherever the shell hook already stands (it appends the same entry); a decline,
    /// `auto_install = false` or the lanes off, with no hook, hand nothing.
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
        // The hook already appends `bin/` in every shell that sources it.
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
