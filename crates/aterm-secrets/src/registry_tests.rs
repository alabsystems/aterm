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

/// A vault this Mac created, holding `entries` (name, value) in `[everyone]`.
fn vault(root: &std::path::Path, entries: &[(&str, &[u8])]) -> crate::client::Paths {
    use crate::client::{admin_open, admin_publish, create};
    let paths = crate::client::Paths::for_home(&root.join("m1"));
    create(
        &paths,
        root.join("team.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    for (name, value) in entries {
        doc.set_entry(&crate::text::Audience::Everyone, name, value)
            .unwrap();
    }
    admin_publish(&paths, &admin, &doc).unwrap();
    paths
}

/// The registry is the verified view's names and purposes, never a value, a digest or a file:
/// nothing under the home is read, so a file an older build left at an entry's old path neither
/// adds nor hides a name.
#[test]
fn the_registry_reads_names_and_purposes_from_the_store_and_no_value() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = vault(
        root.path(),
        &[
            ("release-notary", b"notary-secret-value"),
            ("gh_access_token", b"token-secret-value"),
            ("~/.aterm/apple/devid-a1.cer", b"certificate-value"),
        ],
    );
    crate::fsx::ensure_private_dir(&paths.home.join(".secrets")).unwrap();
    crate::fsx::write_atomic(&paths.home.join(".secrets/stray"), b"old", 0o600).unwrap();
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
    assert_eq!(entries[1].purpose, purpose("release-notary"));
    assert_eq!(entries[2].purpose, "Apple Developer ID certificate");
    let all = format!("{entries:?}");
    assert!(
        !all.contains("secret-value") && !all.contains("certificate-value"),
        "{all}"
    );
    assert_eq!(
        held(&paths, &Want::Notary).unwrap().as_deref(),
        Some("release-notary")
    );
    assert_eq!(held(&paths, &Want::SiteDeployerKey).unwrap(), None);
}

/// NEGATIVE CONTROL: a Mac that applied no registry is no empty registry, and neither is one
/// whose view cannot be verified (a state file with no vault behind it). Its read says why, so
/// the harness says the vault was not read rather than that it holds nothing.
#[test]
fn a_mac_with_no_applied_registry_is_unread_never_empty() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = crate::client::Paths::locate_for_home(home.path());
    assert_eq!(read(&paths), Err(NO_REGISTRY.to_owned()));
    let paths = state(home.path(), "this is [not toml");
    let why = read(&paths).unwrap_err();
    assert_ne!(why, NO_REGISTRY);
    // A hand-written manifest names nothing: names come from the verified view alone.
    let paths = state(home.path(), "[manifest]\nrelease-notary = 'digest'\n");
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
