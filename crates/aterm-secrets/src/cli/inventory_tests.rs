// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn registry_listing_does_not_require_a_readable_credential_identity() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = crate::client::Paths::for_home(home.path());
    crate::fsx::ensure_private_dir(&paths.dir).unwrap();
    crate::fsx::write_atomic(&paths.dir.join("identity"), b"not an age identity", 0o600).unwrap();
    crate::fsx::write_atomic(
        &paths.dir.join("state.toml"),
        format!(
            "own = ['{SITE_DEPLOYER_KEY_ENTRY}', 'release-notary']\n[manifest]\nrelease-notary = 'metadata-digest'\n"
        ).as_bytes(),
        0o600,
    )
    .unwrap();
    assert!(crate::client::status(&paths).is_err());
    let names = crate::client::registered_names(&paths).unwrap();
    assert_eq!(names, ["release-notary", SITE_DEPLOYER_KEY_ENTRY]);
    let text = listing(&names, &[], true, false).unwrap();
    assert!(text.contains("~/.secrets/release-notary"));
    assert!(!text.contains("metadata-digest"));
    assert!(!text.contains("not an age identity"));
    assert!(text.contains(SITE_DEPLOYER_KEY_ENTRY));
}

#[test]
fn metadata_preserves_custom_destinations_and_json_escaping() {
    let names = vec![
        "~/.aterm/release-credentials.toml".into(),
        "quoted\"name".into(),
    ];
    let encoded = listing(&names, &[], false, true).unwrap();
    let parsed: aterm_json::Value = aterm_json::from_str(&encoded).unwrap();
    assert_eq!(
        parsed["entries"][0]["pointer"].as_str(),
        Some("~/.aterm/release-credentials.toml")
    );
    assert_eq!(parsed["entries"][1]["name"].as_str(), Some("quoted\"name"));
    assert!(parsed["entries"][1]["pointer"].is_null());
    assert_eq!(
        listing(&names[..1], &[], false, false).unwrap(),
        "~/.aterm/release-credentials.toml\n"
    );
}
