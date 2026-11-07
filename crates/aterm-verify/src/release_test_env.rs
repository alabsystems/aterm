// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release conditions belong to test children, never to a Background verifier's admission.
//! A nonempty release history catches tests that assume a Mac has never cut a release.

use crate::{Ctx, exec::Cmd};
use std::path::PathBuf;

fn home(ctx: &Ctx) -> PathBuf {
    ctx.scratch.join("release-test-home")
}

/// Prepare a disposable home, including a persisted successful release's footprint shape.
/// The deliberately large record also catches assumptions about the warm disk floor.
pub(crate) fn prepare(ctx: &Ctx) -> std::io::Result<()> {
    if !ctx.checkers_in_tests {
        return Ok(());
    }
    let state = home(ctx).join(".local/state/aterm-release");
    std::fs::create_dir_all(&state)?;
    let mut history = aterm_capacity::Footprints::default();
    history.push(aterm_capacity::Footprint {
        peak: 64 << 30,
        grew: 64 << 30,
        at_ms: 1,
        dirs: None,
        scope: atpkg::admit_disk::scope_tag("release"),
    });
    history.save(&state.join("disk-footprint.json"))
}

/// Explicit release policy, shared by Cargo fallback, replayed binaries and doctests.
/// Cache locations stay those selected before HOME is replaced; no credentials are copied.
pub(crate) fn command(ctx: &Ctx, mut cmd: Cmd) -> Cmd {
    cmd.envs
        .retain(|(key, _)| key != aterm_capacity::RELEASE_RUN_ENV);
    if ctx.checkers_in_tests {
        cmd = cmd
            .env(aterm_capacity::RELEASE_RUN_ENV, "1")
            .env("HOME", home(ctx))
            .env(
                "ATERM_TEST_TOOL_HOME",
                ctx.env
                    .tool_home
                    .clone()
                    .unwrap_or_else(|| ctx.env.home.clone()),
            )
            .env(
                "CARGO_HOME",
                ctx.env
                    .cargo_home
                    .clone()
                    .unwrap_or_else(|| ctx.env.home.join(".cargo")),
            )
            .env(
                "RUSTUP_HOME",
                aterm_toolchain::rustup::rustup_home(ctx.env.rustup_home.as_deref(), &ctx.env.home),
            );
        if let Some(prefix) = &ctx.env.test_package_prefix {
            cmd = cmd.env("ATERM_TEST_PACKAGE_PREFIX", prefix);
        }
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EnvSnapshot, Scope, exec};

    fn context(root: &std::path::Path, release: bool) -> Ctx {
        let env = EnvSnapshot {
            home: root.join("owner-home"),
            tool_home: Some(root.join("installed-tools")),
            test_package_prefix: Some(root.join("signed-store")),
            path: std::env::var_os("PATH").unwrap_or_default(),
            ..EnvSnapshot::default()
        };
        Ctx::new(root.into(), Scope::workspace(), env, root.join("scratch"))
            .with_checkers_in_tests(release)
    }

    #[test]
    fn a_nonempty_history_is_injected_without_reading_the_owners_state() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let ordinary = context(dir.path(), false);
        prepare(&ordinary).unwrap();
        assert!(!home(&ordinary).exists());
        let release = context(dir.path(), true);
        prepare(&release).unwrap();
        let record = home(&release).join(".local/state/aterm-release/disk-footprint.json");
        let history = aterm_capacity::Footprints::load(&record);
        assert_eq!(history.runs().len(), 1);
        assert_eq!(history.runs()[0].grew, 64 << 30);
        assert!(!release.env.home.exists());
        let cmd = command(&release, Cmd::new("probe"));
        let value = |key: &str| {
            cmd.envs
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(value("HOME"), Some(home(&release).into_os_string()));
        assert_eq!(
            value("ATERM_TEST_TOOL_HOME"),
            release.env.tool_home.clone().map(PathBuf::into_os_string)
        );
        assert_eq!(
            value("ATERM_TEST_PACKAGE_PREFIX"),
            release
                .env
                .test_package_prefix
                .clone()
                .map(PathBuf::into_os_string)
        );
        assert_eq!(
            value("CARGO_HOME"),
            Some(release.env.home.join(".cargo").into_os_string())
        );
        assert_eq!(
            value("RUSTUP_HOME"),
            Some(release.env.home.join(".rustup").into_os_string())
        );
    }

    /// NEGATIVE CONTROL: a planted child fails only in release conditions. Removing the
    /// explicit policy makes it pass, which must fail this test rather than hide the red.
    #[cfg(unix)]
    #[test]
    fn a_planted_release_only_red_is_seen_by_the_lanes_test_policy() {
        const PROBE: &str = "ATERM_RELEASE_POLICY_PROBE";
        if let Some(mode) = std::env::var_os(PROBE) {
            let marker = std::env::var_os(aterm_capacity::RELEASE_RUN_ENV);
            if mode == "red" {
                assert!(marker.is_none());
            } else {
                assert_eq!(marker.as_deref(), Some(std::ffi::OsStr::new("1")));
            }
            return;
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        for (release, mode, passes) in [
            (false, "red", true),
            (true, "red", false),
            (true, "fixed", true),
        ] {
            let ctx = context(dir.path(), release);
            std::fs::create_dir_all(&ctx.scratch).unwrap();
            prepare(&ctx).unwrap();
            let probe = Cmd::new(std::env::current_exe().unwrap())
                .args(["--exact", "release_test_env::tests::a_planted_release_only_red_is_seen_by_the_lanes_test_policy"])
                .env(PROBE, mode)
                .env(aterm_capacity::RELEASE_RUN_ENV, "poisoned inherited policy");
            let out = exec::run(&command(&ctx, probe), ctx.exec_env());
            assert_eq!(
                out.ok, passes,
                "release={release}, mode={mode}: {}",
                out.output
            );
            assert!(
                out.output
                    .contains(if passes { "1 passed" } else { "1 failed" }),
                "{}",
                out.output
            );
        }
    }
}
