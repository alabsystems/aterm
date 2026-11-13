// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn store_lock_is_an_flock_not_a_file_lease() {
    // brew leaves its flock file behind forever; its mere
    // existence must never block an abrew writer, a held
    // flock must, and dropping the holder releases it.
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.lock");
    std::fs::write(&path, b"").unwrap();
    let first =
        aterm_update_core::FileLock::acquire_within(&path, std::time::Duration::from_secs(1))
            .expect("an existing unlocked lock file does not block");
    assert!(
        aterm_update_core::FileLock::acquire_within(&path, std::time::Duration::ZERO,).is_err(),
        "a held flock still excludes a second writer"
    );
    drop(first);
    aterm_update_core::FileLock::acquire_within(&path, std::time::Duration::from_secs(1))
        .expect("dropping the holder releases the flock");
}

#[test]
fn auto_trust_kind_mirrors_brew_qualified_rules() {
    let facts = |official, canonical: Option<&str>, formula_file, cask_file| AutoTrustFacts {
        official,
        canonical_formula: canonical.map(str::to_owned),
        formula_file,
        cask_file,
    };
    // Direct canonical spelling with both files: formula wins
    // untyped; --cask forces the cask; --formula the formula.
    let both = facts(false, Some("custool"), true, true);
    assert_eq!(
        auto_trust_kind(None, "custool", &both),
        Some(TrustType::Formula)
    );
    assert_eq!(
        auto_trust_kind(Some(TrustType::Cask), "custool", &both),
        Some(TrustType::Cask)
    );
    assert_eq!(
        auto_trust_kind(Some(TrustType::Formula), "custool", &both),
        Some(TrustType::Formula)
    );
    // Cask-only tap entry under an untyped name.
    let cask_only = facts(false, Some("custapp"), false, true);
    assert_eq!(
        auto_trust_kind(None, "custapp", &cask_only),
        Some(TrustType::Cask)
    );
    assert_eq!(
        auto_trust_kind(Some(TrustType::Formula), "custapp", &cask_only),
        None
    );
    // Official taps and missing files never auto-trust; case
    // in the typed spelling still counts as canonical.
    assert_eq!(
        auto_trust_kind(None, "custool", &facts(true, Some("custool"), true, true)),
        None
    );
    assert_eq!(
        auto_trust_kind(
            None,
            "custool",
            &facts(false, Some("custool"), false, false)
        ),
        None
    );
    assert_eq!(
        auto_trust_kind(None, "CusTool", &both),
        Some(TrustType::Formula)
    );
    // Alias/rename spellings never auto-trust (brew measured:
    // the factory refusal names the canonical item instead).
    let alias = facts(false, Some("custool"), true, false);
    assert_eq!(auto_trust_kind(None, "custalias", &alias), None);
    assert_eq!(
        auto_trust_kind(Some(TrustType::Formula), "custold", &alias),
        None
    );
}

#[test]
fn normalize_remote_matches_brew_rules() {
    assert_eq!(
        normalize_remote("git@github.com:Someone/Homebrew-Tools.git"),
        "https://github.com/someone/homebrew-tools"
    );
    assert_eq!(
        normalize_remote("https://github.com/Someone/Tools/"),
        "https://github.com/someone/tools"
    );
    // `.git` is only insignificant on known hosts.
    assert_eq!(
        normalize_remote("https://example.com/Someone/Tools.git"),
        "https://example.com/someone/tools.git"
    );
    assert!(same_remote(
        "git@github.com:someone/tools",
        "https://github.com/someone/tools.git"
    ));
    assert!(!same_remote(
        "https://example.com/someone/tools",
        "https://example.com/someone/tools.git"
    ));
    assert!(remote_reference("https://example.com/x"));
    assert!(remote_reference("/tmp/x"));
    assert!(!remote_reference("someone/tools"));
}

#[test]
fn store_order_and_pretty_follow_insertion() {
    let pairs: StorePairs = vec![
        (
            "trustedtaps".into(),
            aterm_json::Value::Array(vec![aterm_json::Value::String("a/b".into())]),
        ),
        (
            "trustedformulae".into(),
            aterm_json::Value::Array(vec![aterm_json::Value::String("a/b/c".into())]),
        ),
    ];
    assert_eq!(
        pretty_pairs(&pairs),
        "{\n  \"trustedtaps\": [\n    \"a/b\"\n  ],\n  \"trustedformulae\": [\n    \"a/b/c\"\n  ]\n}"
    );
    assert_eq!(
        top_level_keys(b"{\"trustedcasks\":[],\"trustedtaps\":[\"x/y\"]}"),
        ["trustedcasks", "trustedtaps"]
    );
}
