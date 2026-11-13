// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_a_settings_app_leaf_with_a_retained_view_identifies_the_target() {
    let leaf = "    leaf path=/ view=42 rect_px=0,20,584,348 focused=true kind=app app=settings";
    assert_eq!(settings_view(leaf).as_deref(), Some("42"));
    for other in [
        leaf.replace("app=settings", "app=workers"),
        leaf.replace("kind=app", "kind=terminal"),
        leaf.replace("view=42", "view=unknown"),
        leaf.replace("view=42", "view=0"),
        leaf.replace("leaf ", "tab "),
    ] {
        assert_eq!(settings_view(&other), None);
    }
}

#[test]
fn focus_witness_requires_the_exact_control_and_true_field() {
    let tree = "node key=\"settings/control/background_material\" role=Combobox state=enabled=true,focused=true,focus-visible=true\n";
    assert!(focused(tree, "settings/control/background_material"));
    assert!(!focused(tree, "settings/control/background_opacity"));
    assert!(!focused(
        &tree.replace("focused=true", "focused=false"),
        "settings/control/background_material"
    ));
    assert!(!focused(
        "node key=\"settings/control/background_material\" label=\"focused=true\" state=-",
        "settings/control/background_material"
    ));
}

#[test]
fn settings_observation_requires_explicit_frozen_binary_identities() {
    let args = vec![
        "--source".into(),
        "a".repeat(40),
        "--gui-sha".into(),
        "b".repeat(64),
        "--ctl-sha".into(),
        "c".repeat(64),
        "--backend".into(),
        "cpu".into(),
        "--out".into(),
        "/tmp/.ubd/settings.noindex".into(),
    ];
    assert!(parse(&args).is_ok());
    for (index, invalid) in [
        (1, "unknown"),
        (3, "unverified"),
        (5, ""),
        (7, "auto"),
        (9, "relative"),
    ] {
        let mut changed = args.clone();
        changed[index] = invalid.into();
        assert!(parse(&changed).is_err());
    }
}

#[test]
fn unknown_version_does_not_become_source_proof_or_known_mismatch() {
    let source = "1234567890abcdef1234567890abcdef12345678";
    assert!(version_matches("OK commit=1234567890ab", source).is_ok());
    assert!(version_matches("OK commit=unknown", source).is_ok());
    assert!(version_matches("OK version=unknown", source).is_ok());
    assert!(version_matches("OK commit=abcdef012345", source).is_err());
    assert!(version_matches("OK commit=1234567890ab-dirty", source).is_err());
}

#[test]
fn bool_witness_is_bound_to_exact_semantic_identity() {
    let tree = "node key=\"settings/control/a\" role=Switch value=false state=enabled=true\nnode key=\"settings/control/b\" role=Switch value=true state=enabled=true\n";
    assert_eq!(bool_control(tree, "settings/control/a"), Some(false));
    assert_eq!(bool_control(tree, "settings/control/b"), Some(true));
    assert_eq!(bool_control(tree, "settings/control/missing"), None);
    assert_eq!(
        bool_control(
            "node key=\"settings/control/a\" value=-",
            "settings/control/a"
        ),
        None
    );
}
