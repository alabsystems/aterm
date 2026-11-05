// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

fn recipe() -> (aterm_tempfile::TempDir, RootRecipe) {
    let world = aterm_tempfile::Builder::new()
        .prefix("epoch-recipe-")
        .tempdir()
        .unwrap();
    let sdk = world.path().canonicalize().unwrap().join("sdk");
    std::fs::create_dir(&sdk).unwrap();
    // Private metadata fixtures, never executable toolchain qualifications or child images.
    for name in ["trustc", "trustdoc"] {
        let file = sdk.join(name);
        std::fs::write(&file, b"not a compiler; never executed").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    let recipe = RootRecipe {
        id: 0,
        profile: RootProfile::Dev,
        bin: "example_bin".into(),
        jobs: 2,
        package: None,
        features: vec!["dep/feature".into()],
        default_features: false,
        all_features: false,
        offline: true,
        content_inputs_v1: false,
        analysis: None,
        version: "0.1.0+gabcdef012".into(),
        rustc: sdk.join("trustc"),
        rustdoc: sdk.join("trustdoc"),
        path: vec![sdk],
    };
    (world, recipe)
}

#[test]
fn startup_encoding_keeps_nullable_package_and_rejects_aliases_and_mutable_drivers() {
    let (_world, recipe) = recipe();
    let encoded = encoded(std::slice::from_ref(&recipe), 1).unwrap();
    let value: aterm_json::Value = aterm_json::from_str(&encoded[0]).unwrap();
    assert_eq!(
        value.get("schema").and_then(aterm_json::Value::as_u64),
        Some(1)
    );
    assert_eq!(value.get("package"), Some(&aterm_json::Value::Null));
    assert!(value.get("content_inputs_v1").is_none());
    assert!(value.get("build_analysis").is_none());
    let mut analyzed = recipe.clone();
    analyzed.analysis = Some(RootAnalysis {
        cargo_home: _world.path().canonicalize().unwrap(),
    });
    let analysis = super::encoded(&[analyzed.clone()], 1).unwrap();
    let analysis: aterm_json::Value = aterm_json::from_str(&analysis[0]).unwrap();
    assert_eq!(
        analysis["build_analysis"]["cargo_home"].as_str(),
        analyzed.analysis.as_ref().unwrap().cargo_home.to_str()
    );
    analyzed.analysis.as_mut().unwrap().cargo_home = "relative-cache-home".into();
    assert!(RootRecipe::validate_set(&[analyzed], 1).is_err());
    let mut complete = recipe.clone();
    complete.content_inputs_v1 = true;
    let configured = super::encoded(&[complete], 1).unwrap();
    let configured: aterm_json::Value = aterm_json::from_str(&configured[0]).unwrap();
    assert_eq!(
        configured
            .get("content_inputs_v1")
            .and_then(aterm_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        value.get("profile").and_then(aterm_json::Value::as_str),
        Some("dev")
    );
    assert!(RootRecipe::validate_set(&[recipe.clone(), recipe.clone()], 2).is_err());
    let mut foreign = recipe.clone();
    foreign.id = 1;
    assert!(RootRecipe::validate_set(&[foreign], 1).is_err());
    let mut authority = recipe.clone();
    authority.features = vec!["--config".into()];
    assert!(RootRecipe::validate_set(&[authority], 1).is_err());
    std::fs::set_permissions(&recipe.rustc, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(RootRecipe::validate_set(&[recipe], 1).is_err());
}

#[test]
fn version_grammar_rejects_noncanonical_or_incomplete_semver() {
    for value in ["0.1.0", "1.2.3-alpha.1+build.001", "0.1.0+gabcdef012"] {
        assert!(semver(value), "{value}");
    }
    for value in [
        "1.2",
        "01.2.3",
        "1.2.3-01",
        "1.2.3+",
        "1.2.3++x",
        "1.2.3-x..y",
        "1.2.3\n",
    ] {
        assert!(!semver(value), "{value:?}");
    }
}

fn output(world: &Path, recipe: RootRecipe) -> RootOutput {
    let target = world.canonicalize().unwrap().join("target");
    let mut lease = crate::epoch_target::TargetLease::acquire(&target).unwrap();
    let log = |side: &str, bytes: &[u8]| {
        let path = world.join(side);
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        File::open(path).unwrap()
    };
    RootOutput {
        _target_lease: lease.loan().unwrap(),
        logs: CapturedLogs {
            stdout: log("stdout", b"actual-json\n"),
            stderr: log("stderr", b"diagnostic\n"),
        },
        recipe,
        manifest: world.join("Cargo.toml"),
        target,
        epoch: "a".repeat(32),
        outcome: RootOutcome {
            id: 0,
            pid: 123,
            code: Some(0),
            success: true,
        },
        cancellation: None,
    }
}

#[test]
fn complete_reader_uses_original_inodes_and_keeps_execution_identity() {
    let (world, recipe) = recipe();
    let receipt = output(world.path(), recipe.clone());
    std::fs::rename(
        world.path().join("stdout"),
        world.path().join("original-stdout"),
    )
    .unwrap();
    std::fs::write(world.path().join("stdout"), b"foreign replacement").unwrap();
    let result = receipt.read(23).unwrap();
    assert_eq!(result.stdout, b"actual-json\n");
    assert_eq!(result.stderr, b"diagnostic\n");
    assert_eq!(result.recipe, recipe);
    assert_eq!(result.manifest, world.path().join("Cargo.toml"));
    assert_eq!(
        result.target,
        world.path().canonicalize().unwrap().join("target")
    );
    assert_eq!(result.outcome.code, Some(0));
}

#[test]
fn complete_reader_rejects_overflow_and_changed_original_privacy() {
    let (world, recipe) = recipe();
    assert!(output(world.path(), recipe.clone()).read(21).is_err());
    let receipt = output(world.path(), recipe);
    std::fs::set_permissions(
        world.path().join("stderr"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(receipt.read(64).is_err());
}
