// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::DirBuilderExt as _;

const CASE: &str = "ATERM_VERIFY_RUNTIME_SELECTOR_CASE";
const ROOT: &str = "ATERM_VERIFY_RUNTIME_SELECTOR_ROOT";
const POLICY: &str = "ATERM_VERIFY_RUNTIME_SELECTOR_POLICY";

fn private_directory(path: &Path) {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .unwrap();
}

fn child_context(case: &str, root: PathBuf) {
    let home = root.parent().unwrap().to_path_buf();
    let automatic = root.join(aterm_toolchain::build_cache::DEFAULT_TARGET);
    let selectors: [Option<OsString>; 4] = match case {
        "target" => [
            Some(root.join("caller-target.noindex").into_os_string()),
            None,
            None,
            None,
        ],
        "secondary" => [None, Some("secondary.noindex".into()), None, None],
        "build-dir" => [None, None, Some("caller-build.noindex".into()), None],
        "template" => [
            None,
            None,
            Some("{workspace-root}/caller-build.noindex".into()),
            None,
        ],
        "cargo-home" => [None, None, None, Some("cargo-home".into())],
        "compiler-control" => [
            Some(automatic.clone().into_os_string()),
            None,
            None,
            Some(home.join(".cargo").into_os_string()),
        ],
        "default" | "configured" | "unset" => [None, None, None, None],
        _ => panic!("unknown selector case {case}"),
    };
    for (name, expected) in CARGO_LAYOUT_VARS.into_iter().zip(selectors) {
        assert_eq!(std::env::var_os(name), expected, "{case}: {name}");
    }
    assert_eq!(std::env::current_dir().unwrap(), root);
    assert_eq!(
        std::env::var_os("CARGO_MANIFEST_DIR"),
        Some(root.join("crates/aterm-verify").into_os_string())
    );
    assert!(std::env::var_os("OUT_DIR").is_none());
    assert_eq!(
        std::env::var_os(POLICY),
        Some("retained-tool-policy".into())
    );

    let env = EnvSnapshot::capture();
    assert_eq!(env.home, home);
    let ctx = Ctx::new(root.clone(), Scope::workspace(), env, root.join("scratch"));
    let plan = ctx.native_build_plan().unwrap();
    let expected_target = match case {
        "target" => root.join("caller-target.noindex"),
        "secondary" => root.join("secondary.noindex"),
        "cargo-home" => root.join("home-target.noindex"),
        "configured" => root.join("configured-target.noindex"),
        _ => automatic.clone(),
    };
    assert_eq!(ctx.target_dir, expected_target, "{case}");
    assert_eq!(plan.target(), expected_target, "{case}");
    let generated_final = matches!(case, "default" | "unset" | "build-dir" | "template");
    assert_eq!(
        plan.default_root.as_deref(),
        generated_final.then_some(root.as_path()),
        "{case}: final-target provenance"
    );
    let caller_intermediates =
        matches!(case, "build-dir" | "template" | "cargo-home" | "configured");
    assert_eq!(
        plan.has_explicit_build_dir(),
        caller_intermediates,
        "{case}"
    );
    let explicit = match case {
        "build-dir" => Some(root.join("caller-build.noindex")),
        "cargo-home" => Some(root.join("home-build.noindex")),
        "configured" => Some(root.join("configured-build.noindex")),
        _ => None,
    };
    assert_eq!(plan.explicit_build_dir(), explicit.as_deref(), "{case}");
    if caller_intermediates {
        assert!(ctx.main_build_dir.is_none(), "{case}");
        assert!(ctx.cached_lanes_dir.is_none(), "{case}");
    } else if cfg!(target_os = "macos") && matches!(case, "default" | "unset") {
        let shared = aterm_toolchain::build_cache::cache_root(&home);
        assert_eq!(
            ctx.main_build_dir,
            aterm_toolchain::build_cache::config_value(&shared)
        );
        let selected = shared
            .join(root.strip_prefix("/").unwrap())
            .join(".cargo-build");
        assert_eq!(ctx.cached_lanes_dir, Some(selected.join("verify/.ubd")));
    } else {
        assert_eq!(
            ctx.main_build_dir,
            aterm_toolchain::build_cache::target_build_config(&expected_target),
            "{case}: an actual caller target uses its target fallback"
        );
        assert!(ctx.cached_lanes_dir.is_none(), "{case}");
    }
    assert!(
        !automatic.exists(),
        "context capture prepares no final directory"
    );
}

#[test]
fn a_new_verifier_keeps_original_selectors_in_its_context() {
    const TEST: &str =
        "runtime_selector_tests::a_new_verifier_keeps_original_selectors_in_its_context";
    if let Some(case) = std::env::var_os(CASE) {
        child_context(
            case.to_str().unwrap(),
            PathBuf::from(std::env::var_os(ROOT).unwrap()),
        );
        return;
    }
    for case in [
        "default",
        "target",
        "secondary",
        "build-dir",
        "template",
        "cargo-home",
        "configured",
        "unset",
        "compiler-control",
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = physical(scratch.path()).join("home");
        let root = home.join("source");
        private_directory(&root);
        private_directory(&home.join("Library/Caches"));
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        let mut env = EnvSnapshot {
            home: home.clone(),
            ..EnvSnapshot::default()
        };
        match case {
            "target" => {
                env.cargo_layout
                    .push(("CARGO_TARGET_DIR", "caller-target.noindex".into()));
                // Startup already made the caller's relative target absolute. That captured
                // spelling wins even though the raw field still records the original value.
                env.cargo_target_dir = Some(root.join("caller-target.noindex"));
            }
            "secondary" => env
                .cargo_layout
                .push(("CARGO_BUILD_TARGET_DIR", "secondary.noindex".into())),
            "build-dir" => env
                .cargo_layout
                .push(("CARGO_BUILD_BUILD_DIR", "caller-build.noindex".into())),
            "template" => env.cargo_layout.push((
                "CARGO_BUILD_BUILD_DIR",
                "{workspace-root}/caller-build.noindex".into(),
            )),
            "cargo-home" | "configured" => {
                let (directory, config) = if case == "cargo-home" {
                    env.cargo_layout.push(("CARGO_HOME", "cargo-home".into()));
                    (
                        root.join("cargo-home"),
                        "[build]\ntarget-dir='home-target.noindex'\nbuild-dir='home-build.noindex'\n",
                    )
                } else {
                    (
                        root.join(".cargo"),
                        "[build]\ntarget-dir='configured-target.noindex'\nbuild-dir='configured-build.noindex'\n",
                    )
                };
                private_directory(&directory);
                std::fs::write(directory.join("config.toml"), config).unwrap();
            }
            _ => {}
        }
        let mut command =
            native_verifier::command(&std::env::current_exe().unwrap(), &root, &env).unwrap();
        command
            .args(["--exact", TEST, "--nocapture"])
            .env(CASE, case)
            .env(ROOT, &root)
            .env("HOME", &home)
            .env(POLICY, "retained-tool-policy");
        if case == "unset" {
            for name in CARGO_LAYOUT_VARS {
                command.env(name, root.join("later-ambient-choice"));
            }
            env.apply_cargo_selectors(&mut command);
        }
        if case == "compiler-control" {
            // The former mixed boundary must reproduce the error: an automatically chosen
            // compiler target becomes an explicit runtime target and loses the shared cache.
            NativeBuildPlan::capture(&env, &root)
                .unwrap()
                .apply_to_command(&mut command)
                .unwrap();
        }
        let out = aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        assert!(out.status.success(), "{case}: {out:?}");
    }
}

const CROSS_PHASE: &str = "ATERM_VERIFY_SELECTOR_CAPTURE_PHASE";
const CROSS_CASE: &str = "ATERM_VERIFY_SELECTOR_CAPTURE_CASE";

fn cross_directory_context(case: &str, root: &Path, env: EnvSnapshot) {
    let startup = root.join("subdirectory");
    let expected: [Option<OsString>; 4] = match case {
        "target" => [
            Some(startup.join("final.noindex").into_os_string()),
            None,
            None,
            None,
        ],
        "secondary" => [
            None,
            Some(startup.join("final.noindex").into_os_string()),
            None,
            None,
        ],
        "literal" => [
            None,
            None,
            Some(startup.join("build.noindex").into_os_string()),
            None,
        ],
        "hash" => [
            None,
            None,
            Some(startup.join("build/{workspace-path-hash}").into_os_string()),
            None,
        ],
        "workspace" => [
            None,
            None,
            Some("{workspace-root}/build.noindex".into()),
            None,
        ],
        "cache-home" => [
            None,
            None,
            Some("{cargo-cache-home}/build.noindex".into()),
            Some(startup.join("tools").into_os_string()),
        ],
        "home" => [
            None,
            None,
            None,
            Some(startup.join("tools").into_os_string()),
        ],
        "empty-home" => [None, None, None, Some(OsString::new())],
        "empty-secondary" => [None, Some(OsString::new()), None, None],
        "empty-build" => [None, None, Some(OsString::new()), None],
        _ => panic!("unknown capture case {case}"),
    };
    for (name, expected) in CARGO_LAYOUT_VARS.into_iter().zip(expected) {
        assert_eq!(env.cargo_var(name), expected, "{case}: captured {name}");
    }
    let home = root.parent().unwrap();
    assert_eq!(env.home, home);
    let ctx = Ctx::new(
        root.to_path_buf(),
        Scope::workspace(),
        env,
        root.join("scratch"),
    );
    if matches!(case, "empty-secondary" | "empty-build") {
        assert!(
            ctx.native_build_plan().is_err(),
            "{case}: an empty choice must still refuse"
        );
        return;
    }
    let plan = ctx.native_build_plan().unwrap();
    let expected_target = match case {
        "target" | "secondary" => startup.join("final.noindex"),
        "home" => startup.join("home-final.noindex"),
        _ => root.join(aterm_toolchain::build_cache::DEFAULT_TARGET),
    };
    assert_eq!(
        plan.target(),
        expected_target,
        "{case}: source-root change must not rebase an environment choice"
    );
    let automatic = !matches!(case, "target" | "secondary" | "home");
    assert_eq!(
        plan.default_root.as_deref(),
        automatic.then_some(root),
        "{case}"
    );
    let expected_build = match case {
        "literal" => Some(startup.join("build.noindex")),
        "home" => Some(startup.join("home-build.noindex")),
        _ => None,
    };
    assert_eq!(
        plan.explicit_build_dir(),
        expected_build.as_deref(),
        "{case}"
    );
    assert_eq!(
        plan.has_explicit_build_dir(),
        matches!(
            case,
            "literal" | "hash" | "workspace" | "cache-home" | "home"
        ),
        "{case}"
    );
    let compiler: std::collections::BTreeMap<_, _> = plan.compiler_env().unwrap().collect();
    let cargo_home = if matches!(case, "home" | "cache-home") {
        startup.join("tools")
    } else {
        home.join(".cargo")
    };
    assert_eq!(
        compiler["CARGO_HOME"],
        Some(cargo_home.as_os_str()),
        "{case}"
    );
    assert!(!root.join("decoy-final.noindex").exists());
    assert!(!root.join("decoy-build.noindex").exists());
}

#[test]
fn startup_capture_keeps_relative_selectors_when_the_verifier_changes_directory() {
    const TEST: &str = "runtime_selector_tests::startup_capture_keeps_relative_selectors_when_the_verifier_changes_directory";
    if let Some(phase) = std::env::var_os(CROSS_PHASE) {
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        let case = std::env::var(CROSS_CASE).unwrap();
        let captured = EnvSnapshot::capture();
        cross_directory_context(&case, &root, captured.clone());
        if phase == "observe" {
            assert_eq!(std::env::current_dir().unwrap(), root);
            assert_eq!(
                std::env::var_os("CARGO_MANIFEST_DIR"),
                Some(root.join("crates/aterm-verify").into_os_string())
            );
            return;
        }
        assert_eq!(phase, "capture");
        assert_eq!(std::env::current_dir().unwrap(), root.join("subdirectory"));
        let mut command =
            native_verifier::command(&std::env::current_exe().unwrap(), &root, &captured).unwrap();
        command
            .args(["--exact", TEST, "--nocapture"])
            .env(CROSS_PHASE, "observe");
        let out = aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        assert!(out.status.success(), "{case}: runtime observer {out:?}");
        return;
    }
    for case in [
        "target",
        "secondary",
        "literal",
        "hash",
        "workspace",
        "cache-home",
        "home",
        "empty-home",
        "empty-secondary",
        "empty-build",
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = physical(scratch.path()).join("home");
        let root = home.join("source");
        let startup = root.join("subdirectory");
        private_directory(&startup);
        private_directory(&home.join("Library/Caches"));
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        if matches!(case, "home" | "cache-home") {
            for (directory, config) in [
                (
                    startup.join("tools"),
                    "[build]\ntarget-dir='home-final.noindex'\nbuild-dir='home-build.noindex'\n",
                ),
                (
                    root.join("tools"),
                    "[build]\ntarget-dir='decoy-final.noindex'\nbuild-dir='decoy-build.noindex'\n",
                ),
            ] {
                private_directory(&directory);
                std::fs::write(
                    directory.join("config.toml"),
                    if case == "home" { config } else { "" },
                )
                .unwrap();
            }
        }
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--nocapture"])
            .current_dir(&startup)
            .env("HOME", &home)
            .env(ROOT, &root)
            .env(CROSS_PHASE, "capture")
            .env(CROSS_CASE, case);
        for name in CARGO_LAYOUT_VARS {
            command.env_remove(name);
        }
        match case {
            "target" => {
                command.env("CARGO_TARGET_DIR", "final.noindex");
            }
            "secondary" => {
                command.env("CARGO_BUILD_TARGET_DIR", "final.noindex");
            }
            "literal" => {
                command.env("CARGO_BUILD_BUILD_DIR", "build.noindex");
            }
            "hash" => {
                command.env("CARGO_BUILD_BUILD_DIR", "build/{workspace-path-hash}");
            }
            "workspace" => {
                command.env("CARGO_BUILD_BUILD_DIR", "{workspace-root}/build.noindex");
            }
            "cache-home" => {
                command
                    .env("CARGO_BUILD_BUILD_DIR", "{cargo-cache-home}/build.noindex")
                    .env("CARGO_HOME", "tools");
            }
            "home" => {
                command.env("CARGO_HOME", "tools");
            }
            "empty-home" => {
                command.env("CARGO_HOME", "");
            }
            "empty-secondary" => {
                command.env("CARGO_BUILD_TARGET_DIR", "");
            }
            "empty-build" => {
                command.env("CARGO_BUILD_BUILD_DIR", "");
            }
            _ => unreachable!(),
        }
        let out = aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        assert!(out.status.success(), "{case}: startup capture {out:?}");
    }
}
