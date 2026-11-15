// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::need::catalog::SITE_DEPLOYER_KEY_ENTRY;

/// `ls --describe` names each entry and its purpose, never a file: an entry is read in place.
#[test]
fn a_listing_names_entries_and_purposes_and_no_file() {
    let names = vec![
        "release-notary".to_owned(),
        SITE_DEPLOYER_KEY_ENTRY.to_owned(),
    ];
    let text = listing(&names, &[], true, false).unwrap();
    assert_eq!(
        text,
        format!(
            "release-notary\t{}\n{SITE_DEPLOYER_KEY_ENTRY}\t{}\n",
            description("release-notary"),
            description(SITE_DEPLOYER_KEY_ENTRY)
        )
    );
    assert!(!text.contains("~/.secrets"), "{text}");
}

#[test]
fn metadata_preserves_names_and_json_escaping() {
    let names = vec![
        "~/.aterm/release-credentials.toml".into(),
        "quoted\"name".into(),
    ];
    let encoded = listing(&names, &[], false, true).unwrap();
    let parsed: aterm_json::Value = aterm_json::from_str(&encoded).unwrap();
    assert_eq!(
        parsed["entries"][0]["name"].as_str(),
        Some("~/.aterm/release-credentials.toml")
    );
    assert_eq!(parsed["entries"][1]["name"].as_str(), Some("quoted\"name"));
    assert!(parsed["entries"][1].get("pointer").is_none());
    assert_eq!(
        listing(&names[..1], &[], false, false).unwrap(),
        "~/.aterm/release-credentials.toml\n"
    );
}
