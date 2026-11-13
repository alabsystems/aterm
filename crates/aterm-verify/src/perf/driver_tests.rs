// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(target_os = "macos")]
#[test]
fn a_cached_workspace_plan_prepares_its_original_target_after_config_changes() {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    for unsafe_parent in [false, true] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let temp = temp.path().canonicalize().unwrap();
        let home = temp.join("home");
        let root = home.join("source");
        for path in [&home, &root] {
            std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        std::fs::create_dir(root.join(".cargo")).unwrap();
        let config = root.join(".cargo/config.toml");
        std::fs::write(&config, "[build]\ntarget-dir='old.noindex'\n").unwrap();
        let target = root.join("old.noindex");
        assert!(atpkg::build_cache::registered::create_checked(&home, &target, &root).unwrap());
        // Registration creates the cache's parent. Make the actual shared root unavailable
        // for this selection so the captured plan must exercise its target fallback.
        let shared = aterm_toolchain::build_cache::cache_root(&home);
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o500)).unwrap();
        let env = crate::EnvSnapshot {
            home: home.clone(),
            ..crate::EnvSnapshot::default()
        };
        assert!(crate::main_build_dir(&env, &root).is_none());
        let plan = WorkspaceLayout::resolve(env.clone(), root.clone());
        assert_eq!(
            plan.selected,
            aterm_toolchain::build_cache::target_build_config(&target)
        );
        // Later availability and configuration do not reselect the captured target or cache.
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&config, "[build]\ntarget-dir='new.noindex'\n").unwrap();
        assert!(crate::main_build_dir(&env, &root).is_some());
        let outside = temp.join("outside");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&outside)
            .unwrap();
        std::fs::write(outside.join("keep"), b"outside data").unwrap();
        if unsafe_parent {
            std::os::unix::fs::symlink(&outside, target.join(".ubd")).unwrap();
        }
        let result = plan.prepare("build", plan.selected.as_deref());
        assert_eq!(result.is_ok(), !unsafe_parent);
        assert!(!root.join("new.noindex").exists());
        if !unsafe_parent {
            assert!(aterm_toolchain::build_cache::target_build_path(&target).is_dir());
        }
        assert_eq!(
            std::fs::read(outside.join("keep")).unwrap(),
            b"outside data"
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
        assert_eq!(atpkg::build_cache::registered::roots(&home).len(), 1);
        assert!(
            plan.prepare("build", Some("build.build-dir='fixture-choice'"))
                .is_ok()
        );
    }
}

fn write_launcher_fixture(root: &std::path::Path) {
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"launcher_reuse_fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn value() -> u8 { 7 }\n").unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        r#"fn main() {
    println!("{}|{}", std::env::var("CARGO_MANIFEST_DIR").unwrap(), std::env::var("OUT_DIR").unwrap());
}
"#,
    )
    .unwrap();
    // Intentional compiled-in metadata in this disposable build script reproduces
    // ring's environment fingerprint. Shipped code reads source paths at runtime.
    std::fs::write(
        root.join("build.rs"),
        r#"const _: &str = env!("CARGO_MANIFEST_DIR");
const _: Option<&str> = option_env!("OUT_DIR");
fn main() { println!("cargo:rerun-if-changed=build.rs"); }
"#,
    )
    .unwrap();
}

#[test]
fn nested_build_reuses_artifacts_across_launcher_metadata() {
    const TEST: &str =
        "perf::driver::tests::nested_build_reuses_artifacts_across_launcher_metadata";
    const ROOT: &str = "ATERM_VERIFY_LAUNCHER_FIXTURE";
    const PROGRAM: &str = "ATERM_VERIFY_LAUNCHER_TARGO";
    if let Some(root) = std::env::var_os(ROOT) {
        let root = PathBuf::from(root);
        let driver = CargoDriver {
            program: std::env::var_os(PROGRAM)
                .expect("the parent's pinned targo")
                .into(),
        };
        // The process inherited launcher metadata before the production constructor ran.
        let mut cmd = driver.command("build");
        cmd.args(["--offline", "--message-format=json"])
            .current_dir(&root);
        let output = aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
            .expect("Cargo completed");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::write(root.join("build.json"), output.stdout).unwrap();
        return;
    }
    let driver = cargo_driver()
        .unwrap_or_else(|why| aterm_spec::host::could_not_run(format_args!("pinned targo: {why}")));
    let scratch = aterm_tempfile::tempdir().expect("isolated Cargo fixture");
    let root = scratch.path();
    write_launcher_fixture(root);
    let mut executable = None;
    for (launcher, want_fresh) in [("first", false), ("second", true)] {
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        cmd.args(["--exact", TEST, "--nocapture"])
            .current_dir(root)
            .env(ROOT, root)
            .env(PROGRAM, &driver.program)
            .env("CARGO_TARGET_DIR", root.join("target.noindex"))
            .env("CARGO_BUILD_BUILD_DIR", root.join("build.noindex"))
            .env("CARGO_MANIFEST_DIR", root.join(launcher))
            .env("OUT_DIR", root.join(launcher).join("out"));
        let output = aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
            .expect("the nested Cargo test completed");
        assert!(
            output.status.success(),
            "{launcher}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let built = std::fs::read_to_string(root.join("build.json")).unwrap();
        let mut artifacts = 0;
        for line in built.lines() {
            let Ok(row) = aterm_json::from_str::<aterm_json::Value>(line) else {
                continue;
            };
            if row["reason"].as_str() == Some("compiler-artifact") {
                assert_eq!(
                    row["fresh"].as_bool(),
                    Some(want_fresh),
                    "{launcher}: {row}"
                );
                artifacts += 1;
                if row["target"]["name"].as_str() == Some("launcher_reuse_fixture")
                    && let Some(path) = row["executable"].as_str()
                {
                    executable = Some(PathBuf::from(path));
                }
            }
        }
        assert_eq!(
            artifacts, 3,
            "the fixture's build script, library and binary"
        );
    }
    let mut run = crate::exec::check_command(executable.expect("Cargo's built binary"));
    let runtime_out = root.join("runtime-output");
    run.env("CARGO_MANIFEST_DIR", root)
        .env("OUT_DIR", &runtime_out);
    super::super::bench_build::Targo::new(driver).environment(&mut run);
    let output = aterm_uds::deadline::output_within(&mut run, aterm_spec::wait::MUST_HAPPEN)
        .expect("the runtime fixture completed");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("{}|{}", root.display(), runtime_out.display())
    );
}

#[test]
fn a_refused_stage2_is_not_driven() {
    let stage2 = PathBuf::from("/s/bin");
    let refused = crate::Toolchain {
        stage2_dir: stage2.clone(),
        targo: stage2.join("targo"),
        trustdoc: stage2.join("trustdoc"),
        tippy: None,
        trustfmt: None,
        borrowed: Vec::new(),
        refused: Some(stage2.clone()),
        store_bin: None,
        demoted: None,
        lock: None,
        lock_refusal: None,
        lock_override: None,
    };
    let why = driver_of(&refused).expect_err("a refused directory drives nothing");
    assert!(why.contains("NOT the toolchain"), "{why}");
}

#[test]
fn the_lane_flag_goes_on_compiling_verbs_only() {
    let d = CargoDriver {
        program: PathBuf::from("/s/targo"),
    };
    for verb in LANE_VERBS {
        assert_eq!(CargoDriver::lane_args(verb), &["--unverified"], "{verb}");
    }
    assert_eq!(
        d.display_led("metadata", None, &["--no-deps"]),
        "/s/targo metadata --no-deps"
    );
    assert_eq!(
        d.display_led("run", None, &["--release", "-p", "aterm-bench"]),
        "/s/targo --unverified run --release -p aterm-bench"
    );
    let dir = Some("build.build-dir='/w/target/build.noindex'");
    assert_eq!(
        d.display_led("build", dir, &["--release"]),
        "/s/targo --config build.build-dir='/w/target/build.noindex' --unverified build --release"
    );
    assert_eq!(d.display_led("metadata", dir, &[]), "/s/targo metadata");
}
