// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn state(home: &std::path::Path, text: &str) -> crate::client::Paths {
    let paths = crate::client::Paths::locate_for_home(home);
    crate::fsx::ensure_private_dir(&paths.dir).unwrap();
    crate::fsx::write_atomic(&paths.dir.join("identity"), b"not an age identity", 0o600).unwrap();
    crate::fsx::write_atomic(&paths.dir.join("state.toml"), text.as_bytes(), 0o600).unwrap();
    paths
}

/// The registry is read from `state.toml` alone: names, pointers and purposes, never a digest,
/// the identity or a value.
#[test]
fn the_registry_reads_names_pointers_and_purposes_and_no_value() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = state(
        home.path(),
        "own = ['~/.aterm/apple/devid-a1.cer']\n[manifest]\nrelease-notary = 'metadata-digest'\n\
         gh_access_token = 'other-digest'\n",
    );
    let entries = read(&paths).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "gh_access_token",
            "release-notary",
            "~/.aterm/apple/devid-a1.cer"
        ]
    );
    let notary = &entries[1];
    assert_eq!(notary.pointer.as_deref(), Some("~/.secrets/release-notary"));
    assert_eq!(notary.purpose, purpose("release-notary"));
    assert_eq!(entries[2].purpose, "Apple Developer ID certificate");
    let all = format!("{entries:?}");
    assert!(
        !all.contains("digest") && !all.contains("not an age identity"),
        "{all}"
    );
}

/// NEGATIVE CONTROL: a Mac that applied no registry is no empty registry. Its read says why, so
/// the harness says the vault was not read rather than that it holds nothing.
#[test]
fn a_mac_with_no_applied_registry_is_unread_never_empty() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = crate::client::Paths::locate_for_home(home.path());
    assert_eq!(read(&paths), Err(NO_REGISTRY.to_owned()));
    let paths = state(home.path(), "this is [not toml");
    let why = read(&paths).unwrap_err();
    assert_ne!(why, NO_REGISTRY);
    let paths = state(home.path(), "");
    assert_eq!(read(&paths), Ok(Vec::new()));
}

/// The canonical roster is described as a roster; each 0.145 duplicate as a duplicate recorded
/// for retirement, never as live release material.
#[test]
fn the_canonical_roster_is_a_roster_and_each_legacy_copy_a_duplicate() {
    for name in crate::entry_class::PUBLIC_ROSTER {
        assert_eq!(
            purpose(name),
            "Release machine roster or its signature",
            "{name}"
        );
    }
    for name in crate::entry_class::LEGACY_ROSTER
        .into_iter()
        .chain(crate::entry_class::LEGACY_RELEASE_RING)
    {
        assert!(purpose(name).contains("recorded for retirement"), "{name}");
    }
}
