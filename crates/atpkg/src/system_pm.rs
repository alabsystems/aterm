// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `system-pm` protocol's install lane: a package the PLATFORM'S OWN manager
//! resolves — one row of [`crate::vendor::MANAGER_TABLE`] (`apt`, `dnf`, `brew`,
//! `winget`, `scoop`, and the user-scoped `cargo` and `pipx`) — proven by the row's
//! `provides`, never landing in the store. atpkg is a META package manager here: it keeps
//! the policy (which member, which state, which words) and lets the manager the user
//! already has do the fetching. It NEVER installs a manager — a machine without the row's
//! manager reads the member as `unavailable on <target>: <hint>`, and Homebrew arrives
//! through the `pkg` protocol alone.
//!
//! The flow ([`crate::flow::apply_system_pm`]) owns the decisions — the `provides` probe
//! (present ⇒ nothing runs), the manager lookup ([`manager_on_path`]), and the elevation
//! decision (a row that declares `elevated = true` DEFERS under
//! [`Elevation::Deferred`], the unattended pass: `needs admin — run: aterm pkg install
//! <name>`); this module owns everything between a found manager and a proven install:
//!
//! 1. the argv ([`install_argv`]): the table's template with the package in as ONE
//!    argument and the manager's RESOLVED absolute path in `argv[0]` — the lanes never
//!    search `PATH` for an installer at spawn time;
//! 2. the wrapper: a SYSTEM-WIDE manager (`apt`, `dnf` — [`crate::vendor::Manager::elevated`])
//!    runs under the caller's elevation ([`crate::elevate::elevated_argv`]: `sudo` on the
//!    terminal door, `osascript` on the GUI door); a USER-SCOPED one (`brew`, `winget`,
//!    `scoop`, `cargo`, `pipx`) is NEVER wrapped — Homebrew refuses to run as root, and
//!    the rest install into the user's own tree. A user-scoped row that still declares
//!    `elevated = true` (winget over a machine-scoped installer) only DEFERRED in the
//!    flow; here it is run as the user and the manager raises its own prompt (winget:
//!    `--scope machine`, UAC);
//! 3. the `provides` probe ([`crate::elevate::first_provided`]): a bare name resolves on
//!    `PATH` outside the managed prefix, an absolute path by existing; the first is the
//!    `installed via <manager>: <path>` state's path — the MANAGER's name in the
//!    protocol slot (`installed via apt: /usr/bin/emacs`), since that is what keeps the
//!    member current from now on. From the next pass a `system = "<bin>"` member is then
//!    simply system-satisfied.
//!
//! Every tool runs through the injectable [`Runner`]; no test runs a real manager.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::elevate::{Elevation, Io, Runner};
use crate::manifest::Artifact;
use crate::vendor::Manager;

/// The protocol's spelling.
pub const PROTOCOL: &str = "system-pm";

/// Where `mgr`'s binary ([`Manager::binary`]) is on `path_var` outside the managed
/// `prefix` — through the RAW walk ([`crate::vendor::executable_on_path`]: `cargo` is a
/// manager here even though no shim may take that name; `PATHEXT` on Windows). `None`
/// ⇒ the manager is absent and the member is `unavailable on <target>` on this machine.
#[must_use]
pub fn manager_on_path(prefix: &Path, mgr: &Manager, path_var: Option<&OsStr>) -> Option<PathBuf> {
    crate::vendor::executable_on_path(prefix, mgr.binary(), path_var)
}

/// The exact argv the lane runs: the table's template ([`Manager::install_argv`] —
/// the package as ONE argument, winget's `--scope` from `elevated`) with `argv[0]`
/// re-spelled as `manager_bin`, the manager's resolved absolute path.
#[must_use]
pub fn install_argv(
    mgr: &Manager,
    manager_bin: &Path,
    package: &str,
    elevated: bool,
) -> Vec<String> {
    let mut argv = mgr.install_argv(package, elevated);
    if let Some(first) = argv.first_mut() {
        *first = manager_bin.to_string_lossy().into_owned();
    }
    argv
}

/// The `unavailable on <target>: <hint>` hint for a row whose manager is absent: the
/// index author's own `[programs.<name>].unavailable_hint` first, when there is one,
/// then the fact — which manager is missing, what it would have installed, and that
/// atpkg never installs a manager.
#[must_use]
pub fn missing_manager_hint(mgr: &Manager, package: &str, program_hint: Option<&str>) -> String {
    let mut h = String::new();
    if let Some(p) = program_hint.filter(|p| !p.is_empty()) {
        h.push_str(p);
        h.push_str("; ");
    }
    h.push_str(mgr.name);
    h.push_str(" is not on PATH (the pinned row installs ");
    h.push_str(package);
    h.push_str(" through it, and atpkg never installs a package manager)");
    h
}

/// The lane, from a FOUND manager to a proven install: run the manager (wrapped for
/// `elevation` when the manager is system-wide), then probe `provides`. Never decides
/// elevation — the flow already deferred when the policy said so, and a
/// [`Elevation::Deferred`] here for a system-wide manager is a programming error,
/// refused rather than run unelevated.
///
/// # Errors
/// No elevation policy for a system-wide manager, a manager that could not be spawned
/// or exited non-zero, or an install that left none of the `provides` behind.
pub fn install(
    runner: &dyn Runner,
    elevation: Elevation,
    mgr: &Manager,
    artifact: &Artifact,
    manager_bin: &Path,
    prefix: &Path,
    path_var: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let inner = install_argv(mgr, manager_bin, &artifact.package, artifact.elevated);
    let argv = if mgr.elevated {
        let Some(wrapped) = crate::elevate::elevated_argv(elevation, &inner) else {
            let mut m =
                String::from("the system-pm lane was entered without an elevation policy for ");
            m.push_str(mgr.name);
            m.push_str(" (a system-wide manager) — nothing was run");
            return Err(m);
        };
        wrapped
    } else {
        inner
    };
    // A user-scoped manager under the unattended pass (Deferred) runs with NO stdin:
    // its output still reaches the pass log, but nothing it spawns can wait on a
    // prompt. The explicit door (Sudo / Osascript) inherits the terminal so the
    // manager's own questions — and a cask's `sudo` — reach the user who asked.
    let io = if elevation == Elevation::Deferred {
        Io::Unattended
    } else {
        Io::Inherit
    };
    let ran = runner.run(&argv, io)?;
    if !ran.success() {
        let mut m = String::from(mgr.name);
        m.push_str(" install ");
        m.push_str(&artifact.package);
        m.push_str(" failed");
        if let Some(code) = ran.code {
            m.push_str(" (exit ");
            m.push_str(&crate::dec_u64(u64::from(code.unsigned_abs())));
            m.push(')');
        } else {
            m.push_str(" (killed by a signal)");
        }
        m.push_str(" — nothing recorded; re-run: aterm pkg install <name>");
        return Err(m);
    }
    crate::elevate::first_provided(prefix, &artifact.provides, path_var)
        .ok_or_else(|| crate::elevate::nothing_provided(mgr.name, &artifact.provides))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::elevate::testkit::{Recorder, failed, ok};
    use crate::vendor::manager;
    #[cfg(unix)]
    use std::rc::Rc;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    /// The reference apt row (`emacs`), with `provides` as given.
    fn apt_row(provides: Vec<String>) -> Artifact {
        let mut a = crate::vendor::testkit::pm_row();
        a.provides = provides;
        a
    }

    fn scratch(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-syspm-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    fn lay_exe(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// The argv is the table's template with the package as ONE argument and the
    /// resolved manager path in front — exact, per manager.
    #[test]
    fn the_argv_is_the_template_with_the_resolved_binary_in_front() {
        let apt = manager("apt").unwrap();
        assert_eq!(
            install_argv(apt, Path::new("/usr/bin/apt-get"), "emacs", true),
            s(&["/usr/bin/apt-get", "install", "-y", "emacs"])
        );
        let brew = manager("brew").unwrap();
        assert_eq!(
            install_argv(
                brew,
                Path::new("/opt/homebrew/bin/brew"),
                "--cask emacs",
                false
            ),
            s(&["/opt/homebrew/bin/brew", "install", "--cask emacs"]),
            "the package is ONE argument even when it carries a space (admission refuses \
             such a row; the lane never re-splits)"
        );
        let winget = manager("winget").unwrap();
        assert_eq!(
            install_argv(
                winget,
                Path::new("C:\\Users\\u\\AppData\\Local\\Microsoft\\WindowsApps\\winget.exe"),
                "GNU.Emacs",
                true
            ),
            s(&[
                "C:\\Users\\u\\AppData\\Local\\Microsoft\\WindowsApps\\winget.exe",
                "install",
                "--exact",
                "--id",
                "GNU.Emacs",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--scope",
                "machine"
            ])
        );
        assert_eq!(
            install_argv(
                winget,
                Path::new("/w/winget"),
                "Anthropic.ClaudeCode",
                false
            )[8],
            "user"
        );
        let cargo = manager("cargo").unwrap();
        assert_eq!(
            install_argv(
                cargo,
                Path::new("/home/u/.cargo/bin/cargo"),
                "ripgrep",
                false
            ),
            s(&["/home/u/.cargo/bin/cargo", "install", "ripgrep"])
        );
        assert_eq!(PROTOCOL, "system-pm");
    }

    /// The missing-manager hint names the manager, the package and the rule; the index
    /// author's own hint, when there is one, comes first.
    #[test]
    fn the_missing_manager_hint_names_the_manager_and_the_rule() {
        let apt = manager("apt").unwrap();
        assert_eq!(
            missing_manager_hint(apt, "emacs", None),
            "apt is not on PATH (the pinned row installs emacs through it, and atpkg never \
             installs a package manager)"
        );
        assert_eq!(
            missing_manager_hint(apt, "emacs", Some("")),
            missing_manager_hint(apt, "emacs", None),
            "an empty program hint is no hint"
        );
        assert_eq!(
            missing_manager_hint(
                manager("winget").unwrap(),
                "GNU.Emacs",
                Some("GNU Emacs ships no Windows-on-ARM build")
            ),
            "GNU Emacs ships no Windows-on-ARM build; winget is not on PATH (the pinned row \
             installs GNU.Emacs through it, and atpkg never installs a package manager)"
        );
    }

    /// The manager lookup is the RAW walk: `cargo` (a deny-listed shim name) is found;
    /// a manager inside the managed prefix never counts; absent is `None`.
    #[cfg(unix)]
    #[test]
    fn the_manager_lookup_finds_deny_listed_names_outside_the_prefix() {
        let root = scratch("lookup");
        let prefix = root.join("prefix");
        let sys = root.join("usr-bin");
        let cargo = lay_exe(&sys, "cargo");
        let apt_get = lay_exe(&sys, "apt-get");
        lay_exe(&prefix.join("bin"), "brew");
        let path = std::env::join_paths([prefix.join("bin"), sys.clone()]).unwrap();
        assert_eq!(
            manager_on_path(&prefix, manager("cargo").unwrap(), Some(&path)),
            Some(cargo)
        );
        assert_eq!(
            manager_on_path(&prefix, manager("apt").unwrap(), Some(&path)),
            Some(apt_get),
            "apt's binary is apt-get"
        );
        assert_eq!(
            manager_on_path(&prefix, manager("brew").unwrap(), Some(&path)),
            None,
            "a brew inside the managed prefix is not the system's brew"
        );
        assert_eq!(
            manager_on_path(&prefix, manager("winget").unwrap(), Some(&path)),
            None
        );
        assert_eq!(
            manager_on_path(&prefix, manager("apt").unwrap(), None),
            None
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The TTY path for a SYSTEM-WIDE manager: EXACTLY `sudo <apt-get> install -y emacs`
    /// (inherited), then the provides probe — which the fake manager satisfies by laying
    /// the binary on PATH; the recorded path is the one PATH resolves.
    #[cfg(unix)]
    #[test]
    fn a_system_wide_manager_runs_under_sudo_and_proves_by_a_bare_name_on_path() {
        let root = scratch("apt-ok");
        let prefix = root.join("prefix");
        let sys = root.join("usr-bin");
        let apt_get = lay_exe(&sys, "apt-get");
        let path = std::env::join_paths([sys.clone()]).unwrap();
        let mut rec = Recorder::new(vec![ok("")]);
        let (created_in, created_name) = (sys.clone(), "emacs".to_string());
        rec.on_run = Some(Box::new(move |_argv: &[String]| {
            lay_exe(&created_in, &created_name);
        }));
        let rec = Rc::new(rec);
        let art = apt_row(vec![String::from("emacs"), String::from("/nope/emacs")]);
        let got = install(
            &*rec,
            Elevation::Sudo,
            manager("apt").unwrap(),
            &art,
            &apt_get,
            &prefix,
            Some(&path),
        )
        .unwrap();
        assert_eq!(got, sys.join("emacs"));
        let calls = rec.calls.borrow();
        assert_eq!(calls.len(), 1, "one manager run, nothing else");
        assert_eq!(
            calls[0],
            (
                s(&[
                    "/usr/bin/sudo",
                    &apt_get.to_string_lossy(),
                    "install",
                    "-y",
                    "emacs"
                ]),
                Io::Inherit
            )
        );
        drop(calls);
        // The osascript door wraps the same argv.
        let rec2 = Rc::new(Recorder::new(vec![ok("")]));
        let got2 = install(
            &*rec2,
            Elevation::Osascript,
            manager("apt").unwrap(),
            &art,
            &apt_get,
            &prefix,
            Some(&path),
        )
        .unwrap();
        assert_eq!(got2, sys.join("emacs"));
        let argvs = rec2.argvs();
        assert_eq!(argvs[0][0], "/usr/bin/osascript");
        assert!(
            argvs[0][2].contains("'install' '-y' 'emacs'"),
            "{}",
            argvs[0][2]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A USER-SCOPED manager is never wrapped, whatever the policy: brew runs as the
    /// user under Sudo, under Osascript, AND under Deferred (a user-scoped row needs no
    /// elevation, so the flow never deferred it); an elevated winget row asks winget for
    /// the machine scope and is still not wrapped.
    #[cfg(unix)]
    #[test]
    fn a_user_scoped_manager_is_never_wrapped() {
        let root = scratch("brew-ok");
        let prefix = root.join("prefix");
        let bin = root.join("opt-homebrew-bin");
        let brew = lay_exe(&bin, "brew");
        let path = std::env::join_paths([bin.clone()]).unwrap();
        let mut art = apt_row(vec![String::from("emacs")]);
        art.manager = "brew".into();
        art.package = "emacs".into();
        art.elevated = false;
        for policy in [Elevation::Sudo, Elevation::Osascript, Elevation::Deferred] {
            let mut rec = Recorder::new(vec![ok("")]);
            let created = bin.clone();
            rec.on_run = Some(Box::new(move |_argv: &[String]| {
                lay_exe(&created, "emacs");
            }));
            let rec = Rc::new(rec);
            let got = install(
                &*rec,
                policy,
                manager("brew").unwrap(),
                &art,
                &brew,
                &prefix,
                Some(&path),
            )
            .unwrap();
            assert_eq!(got, bin.join("emacs"));
            assert_eq!(
                rec.argvs(),
                vec![s(&[&brew.to_string_lossy(), "install", "emacs"])],
                "{policy:?}: brew is run as the user, never under sudo"
            );
            // The UNATTENDED pass (Deferred) hands the manager NO stdin, so nothing it
            // spawns can wait on a prompt; the explicit door inherits the terminal.
            let expected_io = if policy == Elevation::Deferred {
                Io::Unattended
            } else {
                Io::Inherit
            };
            assert_eq!(rec.calls.borrow()[0].1, expected_io, "{policy:?}");
            let _ = std::fs::remove_file(bin.join("emacs"));
        }
        // winget over a machine-scoped installer: `--scope machine`, no wrapper.
        let winget = lay_exe(&bin, "winget");
        let mut w = art.clone();
        w.manager = "winget".into();
        w.package = "GNU.Emacs".into();
        w.elevated = true;
        let mut rec = Recorder::new(vec![ok("")]);
        let created = bin.clone();
        rec.on_run = Some(Box::new(move |_argv: &[String]| {
            lay_exe(&created, "emacs");
        }));
        let rec = Rc::new(rec);
        install(
            &*rec,
            Elevation::Sudo,
            manager("winget").unwrap(),
            &w,
            &winget,
            &prefix,
            Some(&path),
        )
        .unwrap();
        let argvs = rec.argvs();
        assert_eq!(argvs[0][0], winget.to_string_lossy());
        assert_eq!(&argvs[0][7..], &s(&["--scope", "machine"])[..]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Refusals, each naming its reason: a manager that exits non-zero (or dies of a
    /// signal), an install that leaves no `provides` behind (naming every entry), a
    /// system-wide manager entered without a policy (nothing runs), a manager that
    /// cannot be spawned.
    #[cfg(unix)]
    #[test]
    fn refusals_name_the_reason_and_run_nothing_they_should_not() {
        let root = scratch("apt-bad");
        let prefix = root.join("prefix");
        let apt_get = root.join("apt-get");
        let path = std::env::join_paths([root.clone()]).unwrap();
        let art = apt_row(vec![String::from("emacs"), String::from("/nope/emacs")]);
        let apt = manager("apt").unwrap();
        // Non-zero exit.
        let rec = Rc::new(Recorder::new(vec![failed(
            100,
            "E: Unable to locate package emacs",
        )]));
        let e = install(
            &*rec,
            Elevation::Sudo,
            apt,
            &art,
            &apt_get,
            &prefix,
            Some(&path),
        )
        .unwrap_err();
        assert_eq!(
            e,
            "apt install emacs failed (exit 100) — nothing recorded; re-run: aterm pkg install <name>"
        );
        // Killed by a signal.
        let rec = Rc::new(Recorder::new(vec![crate::elevate::Ran {
            code: None,
            stdout: String::new(),
            stderr: String::new(),
        }]));
        let e = install(
            &*rec,
            Elevation::Sudo,
            apt,
            &art,
            &apt_get,
            &prefix,
            Some(&path),
        )
        .unwrap_err();
        assert!(e.contains("(killed by a signal)"), "{e}");
        // Success reported, nothing provided: every entry named.
        let rec = Rc::new(Recorder::new(vec![ok("")]));
        let e = install(
            &*rec,
            Elevation::Sudo,
            apt,
            &art,
            &apt_get,
            &prefix,
            Some(&path),
        )
        .unwrap_err();
        assert_eq!(
            e,
            "apt reported success, but none of the provides paths exists: emacs, /nope/emacs"
        );
        // No policy for a system-wide manager: nothing runs.
        let rec = Rc::new(Recorder::new(vec![ok("")]));
        let e = install(
            &*rec,
            Elevation::Deferred,
            apt,
            &art,
            &apt_get,
            &prefix,
            Some(&path),
        )
        .unwrap_err();
        assert!(e.contains("without an elevation policy for apt"), "{e}");
        assert!(rec.argvs().is_empty());
        // A manager that cannot be spawned (the real runner, a path that does not
        // exist): the error names the program.
        let mut b = art.clone();
        b.manager = "brew".into();
        b.elevated = false;
        let e = install(
            &crate::elevate::RealRunner,
            Elevation::Deferred,
            manager("brew").unwrap(),
            &b,
            &root.join("no-such-brew"),
            &prefix,
            Some(&path),
        )
        .unwrap_err();
        assert!(e.starts_with("cannot run "), "{e}");
        assert!(e.contains("no-such-brew"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
