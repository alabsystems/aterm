// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{EnvSnapshot, Scope, exec};

#[cfg(unix)]
#[test]
fn background_test_children_cannot_become_release_preemptors() {
    use aterm_capacity::Machine as _;
    const PROBE: &str = "ATERM_TEST_CAPACITY_ROLE_PROBE";
    if let Some(expected) = std::env::var_os(PROBE) {
        assert_eq!(aterm_capacity::Live.release_run(), expected == "release");
        return;
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut ctx = context(dir.path(), true).with_release(true);
    ctx.background = true;
    std::fs::create_dir_all(&ctx.scratch).unwrap();
    prepare(&ctx).unwrap();
    let probe = Cmd::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "test_env::tests::background_test_children_cannot_become_release_preemptors",
        ])
        .env(PROBE, "ordinary");
    let child = command(&ctx, probe);
    let out = exec::run(&child, ctx.exec_env());
    assert!(
        out.ok,
        "test policy elevated a Background descendant: {}",
        out.output
    );
    // A real release marker must still select Release capacity. Only the fixture creates
    // it here; the verifier's default test environment must not manufacture this authority.
    let control = child
        .env(PROBE, "release")
        .env(aterm_capacity::RELEASE_RUN_ENV, "1");
    let out = exec::run(&control, ctx.exec_env());
    assert!(out.ok, "explicit Release negative control: {}", out.output);
}

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

#[cfg(unix)]
#[test]
fn qualify_red_crosses_the_declared_test_boundary_only_when_selected() {
    const CHILD: &str = "ATERM_QUALIFY_RED_BOUNDARY_CHILD";
    const TEST: &str =
        "test_env::tests::qualify_red_crosses_the_declared_test_boundary_only_when_selected";
    if std::env::var_os(CHILD).is_some() {
        if let Some(marker) = std::env::var_os("ATERM_QUALIFY_RED_MARKER") {
            std::fs::write(marker, b"observed\n").unwrap();
            panic!("deliberate qualify red after the declared environment boundary");
        }
        return;
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    for release in [false, true] {
        let mut ctx = context(dir.path(), release);
        let marker = dir.path().join(format!("red-{release}"));
        ctx.env.qualify_red_marker = Some(marker.clone());
        prepare(&ctx).unwrap();
        let child = command(
            &ctx,
            Cmd::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST])
                .env(CHILD, "1"),
        );
        assert!(!exec::run(&child, ctx.exec_env()).ok);
        assert_eq!(std::fs::read(&marker).unwrap(), b"observed\n");
        // Negative control: the previous boundary discarded this opt-in and silently passed.
        let mut unmarked = child;
        unmarked
            .envs
            .retain(|(key, _)| key != "ATERM_QUALIFY_RED_MARKER");
        assert!(exec::run(&unmarked, ctx.exec_env()).ok);
    }
}

#[test]
fn command_local_cargo_home_and_removal_survive_account_home_replacement() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let ctx = context(temp.path(), false);
    let selected = temp.path().join("selected-cargo-home");
    let cmd = command(&ctx, Cmd::new("probe").env("CARGO_HOME", &selected));
    assert!(
        cmd.envs
            .iter()
            .any(|(name, value)| name == "CARGO_HOME" && value == selected.as_os_str())
    );
    assert!(
        cmd.envs
            .iter()
            .any(|(name, value)| name == "HOME" && value == home(&ctx).as_os_str())
    );
    let removed = command(&ctx, Cmd::new("probe").remove_env("CARGO_HOME"));
    assert!(removed.envs.iter().all(|(name, _)| name != "CARGO_HOME"));
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

/// A provisioned tool belongs to the real tool home, never the disposable account.
/// The inherited-HOME run is the negative control: reverting the boundary makes the
/// planted broken lookup pass and this test fail, in both dev and release mode.
#[cfg(unix)]
#[test]
fn provisioned_tools_require_tool_home_in_dev_and_release() {
    const PROBE: &str = "ATERM_TOOL_HOME_PROBE";
    const TEST: &str = "test_env::tests::provisioned_tools_require_tool_home_in_dev_and_release";
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
    const TEST: &str = "test_env::tests::test_children_do_not_inherit_the_cutters_configuration";
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

/// The real environment-clear boundary must preserve selected C tools while retaining isolation.
#[cfg(unix)]
#[test]
fn native_build_inputs_cross_the_recorded_test_boundary() {
    const PROBE: &str = "ATERM_NATIVE_INPUT_PROBE";
    const TEST: &str = "test_env::tests::native_build_inputs_cross_the_recorded_test_boundary";
    match std::env::var(PROBE).as_deref() {
        Ok("subject") => {
            for (key, expected) in [
                ("SDKROOT", "/selected/stage-sdk"),
                ("CC", "/selected/clang"),
                ("HOST_CXX", "/selected/clang++"),
                ("AR_aarch64_apple_darwin", "/selected/ar"),
                ("RANLIB", "/selected/ranlib"),
                ("DEVELOPER_DIR", "/selected/developer"),
                ("LIBCLANG_PATH", "/selected/lib"),
            ] {
                assert_eq!(std::env::var(key).as_deref(), Ok(expected), "{key}");
            }
            for key in [
                "PUBLISH_AGH",
                "ATERM_RELEASE_CUTTER",
                "RUSTFLAGS",
                "CC_SECRET",
                "CC_MY_SECRET_KEY",
                "CFLAGS_BOGUS_FAKE_TARGET",
                "CC_wasm32_unknown_unknown",
                "UNDECLARED_CALLER_STATE",
                aterm_capacity::RELEASE_RUN_ENV,
            ] {
                assert!(std::env::var_os(key).is_none(), "unexpected {key}");
            }
            return;
        }
        Ok("driver") => {
            let captured = EnvSnapshot::capture().native_toolchain;
            for release in [false, true] {
                let dir = aterm_tempfile::tempdir().unwrap();
                let mut ctx = context(dir.path(), release);
                ctx.env.native_toolchain = captured.clone();
                ctx.checks.push(crate::Check::Cells);
                ctx.cells = vec!["mac-arm".into()];
                // Constructed snapshots cannot smuggle unrelated settings across this boundary.
                ctx.env
                    .native_toolchain
                    .push(("RUSTFLAGS".into(), "forbidden".into()));
                for key in ["CC_MY_SECRET_KEY", "CFLAGS_BOGUS_FAKE_TARGET"] {
                    ctx.env
                        .native_toolchain
                        .push((key.into(), "forbidden".into()));
                }
                prepare(&ctx).unwrap();
                let subject = command(
                    &ctx,
                    Cmd::new(std::env::current_exe().unwrap())
                        .args(["--exact", TEST])
                        .env(PROBE, "subject")
                        .env("SDKROOT", "/selected/stage-sdk"),
                );
                let out = exec::run(&subject, ctx.exec_env());
                assert!(out.ok && out.output.contains("1 passed"), "{}", out.output);
                // Recreate the actual defect: an otherwise exact record loses its selected SDK.
                let mut missing = subject;
                missing.envs.retain(|(name, _)| name != "SDKROOT");
                let out = exec::run(&missing, ctx.exec_env());
                assert!(!out.ok && out.output.contains("1 failed"), "{}", out.output);
            }
            return;
        }
        _ => {}
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let ctx = context(dir.path(), false);
    std::fs::create_dir_all(&ctx.scratch).unwrap();
    let mut driver = Cmd::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST])
        .env(PROBE, "driver");
    for (key, value) in [
        ("SDKROOT", "/selected/caller-sdk"),
        ("CC", "/selected/clang"),
        ("HOST_CXX", "/selected/clang++"),
        ("AR_aarch64_apple_darwin", "/selected/ar"),
        ("RANLIB", "/selected/ranlib"),
        ("DEVELOPER_DIR", "/selected/developer"),
        ("LIBCLANG_PATH", "/selected/lib"),
        ("PUBLISH_AGH", "/owner/authority"),
        ("ATERM_RELEASE_CUTTER", "/owner/cutter"),
        ("RUSTFLAGS", "forbidden"),
        ("CC_SECRET", "unrelated"),
        ("CC_MY_SECRET_KEY", "unrelated"),
        ("CFLAGS_BOGUS_FAKE_TARGET", "unrelated"),
        ("CC_wasm32_unknown_unknown", "unselected"),
        ("UNDECLARED_CALLER_STATE", "absent"),
    ] {
        driver = driver.env(key, value);
    }
    let out = exec::run(&driver, ctx.exec_env());
    assert!(out.ok && out.output.contains("1 passed"), "{}", out.output);
}

#[test]
fn exact_native_records_are_not_overwritten_by_later_tool_selection() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut ctx = context(dir.path(), false);
    ctx.env.native_toolchain = vec![
        ("SDKROOT".into(), "/later/sdk".into()),
        ("CC".into(), "/later/cc".into()),
    ];
    let mut recorded = Cmd::new("recorded").env("SDKROOT", "/recorded/sdk");
    recorded.exact_env = true;
    let child = command(&ctx, recorded);
    assert!(child.exact_env);
    assert_eq!(
        child
            .envs
            .iter()
            .find(|(name, _)| name == "SDKROOT")
            .map(|(_, value)| value.as_os_str()),
        Some(OsStr::new("/recorded/sdk"))
    );
    assert!(child.envs.iter().all(|(name, _)| name != "CC"));
}

#[test]
fn native_input_names_do_not_open_an_inherited_environment_prefix() {
    let targets = vec![
        "aarch64-apple-darwin".into(),
        "x86_64-unknown-linux-gnu".into(),
    ];
    for name in [
        "CC",
        "TARGET_CC",
        "CC_aarch64-apple-darwin",
        "CFLAGS_x86_64_unknown_linux_gnu",
        "RANLIB",
        "SDKROOT",
    ] {
        assert!(native_input(OsStr::new(name), &targets), "{name}");
    }
    for name in [
        "CC_SECRET",
        "CC_MY_SECRET_KEY",
        "CFLAGS_BOGUS_FAKE_TARGET",
        "CC_",
        "CC_a__b",
        "RUSTC",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "PUBLISH_AGH",
        "ATERM_RELEASE_RUN",
        "DYLD_INSERT_LIBRARIES",
    ] {
        assert!(!native_input(OsStr::new(name), &targets), "{name}");
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut ctx = context(dir.path(), false);
    ctx.checks = vec![crate::Check::Cells];
    ctx.cells = vec!["win".into()];
    let selected = native_targets(Some(&ctx));
    assert!(selected.contains(&"x86_64-pc-windows-msvc".into()));
    assert!(!selected.contains(&"wasm32-unknown-unknown".into()));
    assert!(native_input(
        OsStr::new("CC_x86_64_pc_windows_msvc"),
        &selected
    ));
    assert!(!native_input(
        OsStr::new("CC_wasm32_unknown_unknown"),
        &selected
    ));
    ctx.cells = vec!["unknown-cell".into()];
    assert_eq!(
        native_targets(Some(&ctx)),
        [aterm_toolchain::rustup::NATIVE_TRIPLE.to_string()]
    );
}
