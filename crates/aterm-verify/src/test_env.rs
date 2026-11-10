// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One declared environment for dev and release test children, never verifier admission.
//! A nonempty release history catches tests that assume a Mac has never cut a release.

use crate::{Ctx, exec::Cmd};
use std::ffi::OsStr;
use std::path::PathBuf;

fn home(ctx: &Ctx) -> PathBuf {
    ctx.scratch.join("release-test-home")
}

/// Prepare a disposable home, including a persisted successful release's footprint shape.
/// The deliberately large record also catches assumptions about the warm disk floor.
pub(crate) fn prepare(ctx: &Ctx) -> std::io::Result<()> {
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

/// Whether one of [`Ctx::child_env_add`]'s declared stage values belongs in a release test.
///
/// Each name has one reason to cross this boundary: Cargo's physical artifact root, the selected
/// Trust compiler, the test-width ceiling, the verifier's run record, the quiet temporary root,
/// or the stage's capacity cover. A new context variable stays out until it is named here.
fn context_variable(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            "CARGO_TARGET_DIR"
                | "TRUST_STAGE2_BIN"
                | "RUST_TEST_THREADS"
                | "ATERM_VERIFY_RUN"
                | "TMPDIR"
                | aterm_capacity::COVER_ENV
        )
    )
}

/// Windows process launch and temporary-file lookup need these OS locations after `env_clear`.
/// They are named individually; account data and every other caller variable stay out.
#[cfg(windows)]
fn platform_variables(_ctx: &Ctx, mut cmd: Cmd) -> Cmd {
    for key in ["SystemRoot", "ComSpec", "PATHEXT", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            cmd = cmd.env(key, value);
        }
    }
    cmd
}

/// Linux tools need the login runtime location even under the declared test
/// environment. Test binaries redirect this canonical location into their own
/// private scratch; another caller-selected runtime must not cross this boundary.
#[cfg(target_os = "linux")]
fn platform_variables(ctx: &Ctx, mut cmd: Cmd) -> Cmd {
    if let Some(dir) = ctx.env.xdg_runtime_dir.as_deref()
        && aterm_uds::test_binary::is_login_runtime_dir(dir.as_os_str())
        && private_runtime(dir, aterm_uds::peer::our_uid())
    {
        cmd = cmd.env("XDG_RUNTIME_DIR", dir);
    }
    cmd
}

#[cfg(target_os = "linux")]
fn private_runtime(dir: &std::path::Path, owner: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    dir.is_absolute()
        && std::fs::symlink_metadata(dir)
            .is_ok_and(|md| md.is_dir() && md.uid() == owner && md.mode() & 0o7777 == 0o700)
        && std::fs::canonicalize(dir).is_ok_and(|physical| physical == dir)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn platform_variables(_ctx: &Ctx, cmd: Cmd) -> Cmd {
    cmd
}

/// Windows directory discovery reads `USERPROFILE`, while Unix discovery reads `HOME`.
/// Both names point at the same disposable release-test home; the owner's profile stays out.
#[cfg(windows)]
fn home_variables(cmd: Cmd, home: &std::path::Path) -> Cmd {
    cmd.env("HOME", home).env("USERPROFILE", home)
}

#[cfg(not(windows))]
fn home_variables(cmd: Cmd, home: &std::path::Path) -> Cmd {
    cmd.env("HOME", home)
}

/// Turn an inherited stage command into the whole declared release-test environment.
///
/// PATH is the verifier's selected tool path. Context values are restricted by
/// [`context_variable`]. The build width and its admission marker keep a managed build bounded.
/// Command-local values were explicitly chosen by the stage (checker policy, Cargo layout,
/// doctest driver, test width and subprocess slots), rather than inherited from the cutter.
/// An already-exact command is a replay recorded by Cargo after this boundary; Cargo's runtime
/// metadata, binary paths and loader path are therefore part of that declared record.
fn declared(ctx: &Ctx, mut cmd: Cmd) -> Cmd {
    if cmd.exact_env {
        return cmd;
    }
    let local = std::mem::take(&mut cmd.envs);
    cmd.exact_env = true;
    cmd = platform_variables(ctx, cmd.env("PATH", &ctx.path_env));
    for (key, value) in &ctx.child_env_add {
        if context_variable(key) {
            cmd = cmd.env(key, value);
        }
    }
    if let Some(jobs) = &ctx.env.cargo_build_jobs {
        cmd = cmd.env("CARGO_BUILD_JOBS", jobs);
    }
    if let Some(held) = &ctx.env.build_slot_held {
        cmd = cmd.env("BUILD_SLOT_HELD", held);
    }
    for (key, value) in local {
        cmd = cmd.env(key, value);
    }
    cmd
}

/// Test policy shared by dev and release, Cargo's recording pass, replays and doctests.
/// Cache locations stay those selected before HOME is replaced; no credentials are copied.
/// `ATERM_RELEASE_RUN` exercises release conditions at landing too. Checker execution is
/// separately selected by `stages::with_checkers`: only qualification owes the golden suite.
pub(crate) fn command(ctx: &Ctx, mut cmd: Cmd) -> Cmd {
    cmd.envs
        .retain(|(key, _)| key != aterm_capacity::RELEASE_RUN_ENV);
    let disposable_home = home(ctx);
    cmd = home_variables(
        declared(ctx, cmd).env(aterm_capacity::RELEASE_RUN_ENV, "1"),
        &disposable_home,
    )
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
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EnvSnapshot, Scope, exec};

    #[cfg(target_os = "linux")]
    #[test]
    fn a_linux_runtime_is_owned_private_and_the_login_location() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let dir = aterm_tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        std::fs::create_dir(&runtime).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let owner = aterm_uds::peer::our_uid();
        assert!(private_runtime(&runtime, owner));
        assert!(!private_runtime(&runtime, owner.wrapping_add(1)));
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(!private_runtime(&runtime, owner));
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = dir.path().join("link");
        symlink(&runtime, &link).unwrap();
        assert!(!private_runtime(&link, owner));
        assert!(!private_runtime(std::path::Path::new("relative"), owner));
        let mut ctx = context(dir.path(), false);
        ctx.env.xdg_runtime_dir = Some(runtime);
        let cmd = command(&ctx, Cmd::new("probe"));
        assert!(cmd.envs.iter().all(|(key, _)| key != "XDG_RUNTIME_DIR"));
    }

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
        let mut release = context(dir.path(), true);
        release.env.cargo_build_jobs = Some("3".into());
        release.env.build_slot_held = Some("1".into());
        release
            .child_env_add
            .push(("UNDECLARED_CALLER_STATE".into(), "absent".into()));
        prepare(&release).unwrap();
        let record = home(&release).join(".local/state/aterm-release/disk-footprint.json");
        let history = aterm_capacity::Footprints::load(&record);
        assert_eq!(history.runs().len(), 1);
        assert_eq!(history.runs()[0].grew, 64 << 30);
        assert!(!release.env.home.exists());
        let cmd = command(&release, Cmd::new("probe"));
        assert!(cmd.exact_env);
        let value = |key: &str| {
            cmd.envs
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(value("HOME"), Some(home(&release).into_os_string()));
        #[cfg(windows)]
        assert_eq!(value("USERPROFILE"), Some(home(&release).into_os_string()));
        assert_eq!(value("PATH"), Some(release.path_env.clone()));
        assert_eq!(
            value("CARGO_TARGET_DIR"),
            Some(release.target_dir.clone().into_os_string())
        );
        assert_eq!(value("CARGO_BUILD_JOBS"), Some("3".into()));
        assert_eq!(value("BUILD_SLOT_HELD"), Some("1".into()));
        assert_eq!(value("UNDECLARED_CALLER_STATE"), None);
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
        release.checkers_in_tests = false;
        prepare(&release).unwrap();
        assert_eq!(command(&release, Cmd::new("probe")), cmd);
    }

    /// NEGATIVE CONTROL: a planted child fails in dev and release conditions. Removing the
    /// explicit policy makes it pass, which must fail this test rather than hide the red.
    #[cfg(unix)]
    #[test]
    fn a_planted_red_is_seen_in_dev_and_release() {
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
            (false, "red", false),
            (false, "fixed", true),
            (true, "red", false),
            (true, "fixed", true),
        ] {
            let ctx = context(dir.path(), release);
            std::fs::create_dir_all(&ctx.scratch).unwrap();
            prepare(&ctx).unwrap();
            let probe = Cmd::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "test_env::tests::a_planted_red_is_seen_in_dev_and_release",
                ])
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

    /// A provisioned tool belongs to the real tool home, never the disposable account.
    /// The inherited-HOME run is the negative control: reverting the boundary makes the
    /// planted broken lookup pass and this test fail, in both dev and release mode.
    #[cfg(unix)]
    #[test]
    fn provisioned_tools_require_tool_home_in_dev_and_release() {
        const PROBE: &str = "ATERM_TOOL_HOME_PROBE";
        const TEST: &str =
            "test_env::tests::provisioned_tools_require_tool_home_in_dev_and_release";
        const TOOL: &str = "Library/Caches/aterm/probe.noindex/tool";
        if let Ok(mode) = std::env::var(PROBE) {
            let root = if mode == "fixed" {
                aterm_uds::host::tool_home().unwrap()
            } else {
                PathBuf::from(std::env::var_os("HOME").unwrap())
            };
            assert!(root.join(TOOL).is_file(), "provisioned tool is absent");
            return;
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        for release in [false, true] {
            let ctx = context(dir.path(), release);
            prepare(&ctx).unwrap();
            let tool_home = ctx.env.tool_home.as_ref().unwrap();
            let tool = tool_home.join(TOOL);
            std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
            std::fs::write(tool, "provisioned").unwrap();
            for (declared, mode, passes) in [
                (false, "broken", true),
                (true, "broken", false),
                (true, "fixed", true),
            ] {
                let probe = Cmd::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST])
                    .env("HOME", tool_home)
                    .env(PROBE, mode);
                let probe = if declared {
                    command(&ctx, probe)
                } else {
                    probe
                };
                let out = exec::run(&probe, ctx.exec_env());
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

    /// NEGATIVE CONTROL: the cutter's configuration is present in the caller, while every
    /// test child receives only the verifier's explicit test policy.
    #[cfg(unix)]
    #[test]
    fn test_children_do_not_inherit_the_cutters_configuration() {
        const PROBE: &str = "ATERM_RELEASE_ENV_PROBE";
        const TEST: &str =
            "test_env::tests::test_children_do_not_inherit_the_cutters_configuration";
        match std::env::var(PROBE).as_deref() {
            Ok("subject") => {
                assert!(std::env::var_os("PUBLISH_AGH").is_none());
                assert!(std::env::var_os("PUBLISH_STAGE_APPEND_ONLY").is_none());
                assert!(std::env::var_os("ATERM_RELEASE_CUTTER").is_none());
                return;
            }
            Ok("driver") => {
                let dir = aterm_tempfile::tempdir().unwrap();
                for release in [false, true] {
                    let ctx = context(dir.path(), release);
                    prepare(&ctx).unwrap();
                    let subject = Cmd::new(std::env::current_exe().unwrap())
                        .args(["--exact", TEST])
                        .env(PROBE, "subject");
                    let out = exec::run(&command(&ctx, subject), ctx.exec_env());
                    assert!(out.ok && out.output.contains("1 passed"), "{}", out.output);
                }
                return;
            }
            _ => {}
        }

        let dir = aterm_tempfile::tempdir().unwrap();
        let ctx = context(dir.path(), false);
        std::fs::create_dir_all(&ctx.scratch).unwrap();
        let driver = Cmd::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST])
            .env(PROBE, "driver")
            .env("PUBLISH_AGH", "/nonexistent")
            .env("PUBLISH_STAGE_APPEND_ONLY", "1")
            .env("ATERM_RELEASE_CUTTER", "/owner/cutter");
        let out = exec::run(&driver, ctx.exec_env());
        assert!(out.ok && out.output.contains("1 passed"), "{}", out.output);
    }
}
