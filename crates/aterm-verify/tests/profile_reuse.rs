// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resolve the workspace's real profiles on tiny dependency and build-tool units.

use std::collections::BTreeMap;

#[test]
fn dev_and_test_resolve_the_same_dependency_profiles() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let manifest: aterm_toml::Value = aterm_toml::from_str(&manifest).unwrap();
    let mut profiles = aterm_toml::Table::new();
    for name in ["dev", "test"] {
        if let Some(profile) = manifest["profile"].get(name) {
            profiles.insert(name.into(), profile.clone());
        }
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let fixture = dir.path();
    for name in ["app", "aterm-effects", "aterm-gpu", "builder"] {
        let package = fixture.join(name);
        std::fs::create_dir_all(package.join("src")).unwrap();
        let deps = if name == "app" {
            "[dependencies]\naterm-effects={path='../aterm-effects'}\naterm-gpu={path='../aterm-gpu'}\n\
             [build-dependencies]\nbuilder={path='../builder'}\n"
        } else {
            ""
        };
        std::fs::write(
            package.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='0.0.0'\nedition='2024'\n{deps}"),
        )
        .unwrap();
        std::fs::write(package.join("src/lib.rs"), "").unwrap();
    }
    std::fs::write(fixture.join("app/build.rs"), "fn main() {}\n").unwrap();
    let config = aterm_toml::to_string(&BTreeMap::from([("profile", profiles)])).unwrap();
    std::fs::write(
        fixture.join("Cargo.toml"),
        format!("[workspace]\nresolver='2'\nmembers=['app','aterm-effects','aterm-gpu','builder']\n{config}"),
    )
    .unwrap();
    let query = |profile: &str, mutation: Option<&str>| {
        let mut cmd = aterm_uds::spawnfd::command(env!("CARGO"));
        cmd.current_dir(fixture).args([
            "--unverified",
            "build",
            "-p",
            "app",
            "--profile",
            profile,
            "--unit-graph",
            "-Z",
            "unstable-options",
            "--offline",
        ]);
        if let Some(mutation) = mutation {
            cmd.args(["--config", mutation]);
        }
        let output =
            aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(60))
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let graph: aterm_json::Value = aterm_json::from_slice(&output.stdout).unwrap();
        graph["units"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|unit| {
                let name = unit["target"]["name"].as_str().unwrap();
                if name == "app" || unit["target"]["kind"][0].as_str() != Some("lib") {
                    return None;
                }
                let mut profile = unit["profile"].as_object().unwrap().clone();
                // Cargo's display name differs; it is not a compiler setting.
                profile.remove("name");
                Some((name.to_owned(), profile))
            })
            .collect::<BTreeMap<_, _>>()
    };
    let dev = query("dev", None);
    assert_eq!(
        dev.len(),
        3,
        "both optimized libraries and the host tool are covered"
    );
    assert_eq!(dev, query("test", None));
    for mutation in [
        "profile.test.debug='line-tables-only'",
        "profile.test.package.aterm-effects.opt-level=0",
        "profile.test.package.aterm-gpu.opt-level=0",
        "profile.test.build-override.debug='line-tables-only'",
    ] {
        assert_ne!(
            dev,
            query("test", Some(mutation)),
            "negative control: {mutation}"
        );
    }
}
