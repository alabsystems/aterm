// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn historical_lean_versions_do_not_change_the_default_or_admit_unknown_inputs() {
    assert_eq!(LEAN4.version, "4.31.0");
    assert!(LEAN4.shim_env.is_empty());
    assert!(LEAN4_430_RC2.shim_env.is_empty());
    assert_eq!(LEAN4_480.shim_env, &["LEAN_CC=/usr/bin/cc"]);
    assert_eq!(LEAN4.versioned("4.31.0"), Some(&LEAN4));
    for (version, recipe) in [("4.8.0", &LEAN4_480), ("4.30.0-rc2", &LEAN4_430_RC2)] {
        assert_eq!(LEAN4.versioned(version), Some(recipe));
        assert_eq!(recipe.name, "lean4");
        assert!(recipe.archive_for("macos-aarch64").is_some());
        assert!(recipe.archive_for("macos-x86_64").is_none());
    }
    assert!(LEAN4.versioned("4.30.0").is_none());
    assert!(GITLEAKS.versioned("4.8.0").is_none());
}

#[test]
fn a_shared_source_archive_does_not_admit_unsupported_hosts() {
    let recipe = PinnedTool {
        hosts: &["macos-aarch64"],
        ..OPENSSL_SIGNING
    };
    assert_eq!(
        recipe.archive_for("macos-aarch64"),
        Some(&OPENSSL_SIGNING_SOURCE)
    );
    for host in ["macos-x86_64", "linux-aarch64", "windows-aarch64", "source"] {
        assert_eq!(recipe.archive_for(host), None, "{host}");
    }
}
