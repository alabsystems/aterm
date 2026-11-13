// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn checkout_compiler_child_receives_the_captured_cargo_layout() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = aterm_verify::physical(scratch.path());
    let repo = home.join("repo");
    aterm_uds::private_file::create_private_dir(&repo).unwrap();
    std::fs::write(repo.join("Cargo.toml"), b"[workspace]\n").unwrap();
    let target = home.join("caller-target");
    let cargo_home = home.join("caller-cargo-home");
    let intermediates = home.join("caller-intermediates");
    let env = aterm_verify::EnvSnapshot {
        home: home.clone(),
        cargo_layout: vec![
            ("CARGO_TARGET_DIR", target.clone().into_os_string()),
            (
                "CARGO_BUILD_TARGET_DIR",
                home.join("superseded-target").into_os_string(),
            ),
            ("CARGO_HOME", cargo_home.clone().into_os_string()),
            (
                "CARGO_BUILD_BUILD_DIR",
                intermediates.clone().into_os_string(),
            ),
        ],
        ..Default::default()
    };
    let bin = Bin {
        package: "agh",
        env: "PUBLISH_AGH",
        what: "GitHub client",
    };
    let ready = prepare_with_jobs(&repo, &bin, None, &env, Some(3), |args, plan| {
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--jobs" && pair[1] == "3")
        );
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--target-dir" && pair[1] == target.as_os_str())
        );
        // macOS printenv accepts one named variable per invocation.
        for (name, expected) in [
            ("CARGO_TARGET_DIR", Some(target.as_path())),
            ("CARGO_BUILD_BUILD_DIR", Some(intermediates.as_path())),
            ("CARGO_HOME", Some(cargo_home.as_path())),
            ("CARGO_BUILD_TARGET_DIR", None),
        ] {
            let mut child = aterm_uds::spawnfd::command("/usr/bin/printenv");
            child.arg(name);
            for name in aterm_verify::CARGO_LAYOUT_VARS {
                child.env(name, home.join("later-value"));
            }
            plan.apply_to_command(&mut child)
                .map_err(|e| e.to_string())?;
            let output = child.output().map_err(|e| e.to_string())?;
            assert_eq!(
                output.status.code(),
                Some(if expected.is_some() { 0 } else { 1 }),
                "{name}"
            );
            assert_eq!(
                output.stdout,
                expected.map_or_else(Vec::new, |path| format!("{}\n", path.display())
                    .into_bytes()),
                "{name}"
            );
        }
        Ok(aterm_json::json!({
            "reason": "compiler-artifact",
            "manifest_path": repo.join("crates/agh/Cargo.toml"),
            "target": {"name": "agh", "kind": ["bin"]},
            "executable": "/bin/echo",
        })
        .to_string()
        .into_bytes())
    })
    .unwrap();
    assert_eq!(ready, Path::new("/bin/echo"));
}

#[test]
fn quiet_checkout_build_refuses_linked_descendants_before_its_compiler_callback() {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

    let bin = Bin {
        package: "agh",
        env: "PUBLISH_AGH",
        what: "GitHub client",
    };
    for shape in [
        "safe",
        "default-target",
        "default-linked-parent",
        "linked-parent",
        "caller-layout",
        "explicit-bin",
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let physical_home = aterm_verify::physical(scratch.path());
        let home = physical_home.as_path();
        let repo = home.join("repo");
        let default_target = matches!(shape, "default-target" | "default-linked-parent");
        let target = repo.join(if default_target {
            aterm_toolchain::build_cache::DEFAULT_TARGET
        } else {
            "target"
        });
        let quiet = target.join(".ubd");
        let outside = home.join("outside");
        aterm_uds::private_file::create_private_dir(&repo).unwrap();
        aterm_uds::private_file::create_private_dir(&outside).unwrap();
        std::fs::write(repo.join("Cargo.toml"), b"[workspace]\n").unwrap();
        assert_eq!(repo.canonicalize().unwrap(), repo);
        if matches!(shape, "safe" | "linked-parent") {
            assert!(!target.exists(), "{shape}: a fresh target");
            assert!(
                atpkg::build_cache::registered::create_checked(home, &target, &repo).unwrap(),
                "{shape}: fresh explicit target registration"
            );
        } else if !default_target {
            aterm_uds::private_file::create_private_dir(&target).unwrap();
        }
        // Keep the ownership index usable while making the actual shared build root
        // unavailable, so this call selects the target-local intermediate plan.
        let shared = aterm_toolchain::build_cache::cache_root(home);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&shared)
            .unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            std::fs::metadata(&shared).unwrap().permissions().mode() & 0o777,
            0o500
        );
        std::fs::write(outside.join("keep"), b"untouched").unwrap();
        if shape == "default-linked-parent" {
            aterm_uds::private_file::create_private_dir(&repo.join("target")).unwrap();
            std::os::unix::fs::symlink(&outside, repo.join("target/.ubd")).unwrap();
        } else if shape != "safe" && !default_target {
            std::os::unix::fs::symlink(&outside, &quiet).unwrap();
        }
        let mut env = aterm_verify::EnvSnapshot {
            home: home.to_path_buf(),
            cargo_target_dir: (!default_target).then(|| target.clone()),
            cargo_layout: if default_target {
                vec![]
            } else {
                vec![("CARGO_TARGET_DIR", target.clone().into_os_string())]
            },
            ..Default::default()
        };
        if shape == "caller-layout" {
            env.cargo_layout
                .push(("CARGO_BUILD_BUILD_DIR", outside.clone().into_os_string()));
        }
        let compiler_ran = std::cell::Cell::new(false);
        let explicit = (shape == "explicit-bin").then_some(OsStr::new("/bin/echo"));
        let result = prepare(&repo, &bin, explicit, &env, |args, _plan| {
            compiler_ran.set(true);
            assert!(args.windows(2).any(|pair| {
                pair[0] == "--target-dir" && pair[1] == aterm_verify::physical(&target).as_os_str()
            }));
            if matches!(shape, "safe" | "default-target") {
                assert!(quiet.join("build.noindex").is_dir());
            } else {
                assert!(args.iter().all(|word| word != "--config"));
            }
            Ok(aterm_json::json!({
                "reason": "compiler-artifact",
                "manifest_path": repo.join("crates/agh/Cargo.toml"),
                "target": {"name": "agh", "kind": ["bin"]},
                "executable": "/bin/echo",
            })
            .to_string()
            .into_bytes())
        });
        assert_eq!(
            result.is_ok(),
            !matches!(shape, "linked-parent" | "default-linked-parent"),
            "{shape}: {result:?}"
        );
        assert_eq!(
            compiler_ran.get(),
            matches!(shape, "safe" | "default-target" | "caller-layout"),
            "{shape}"
        );
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"untouched");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    }
}
