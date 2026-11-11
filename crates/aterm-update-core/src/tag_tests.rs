// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn parse_release_tag_classifies_every_spelling() {
    use TagError::{Malformed, Overflow};
    let rows: &[(&str, Result<TagKind, TagError>)] = &[
        ("v0.10.0", Ok(TagKind::Candidate(vec![0, 10, 0]))),
        ("v0.61", Ok(TagKind::Legacy)),
        ("0.10.0", Err(Malformed)),
        ("V0.10.0", Err(Malformed)),
        ("v", Err(Malformed)),
        ("v0", Err(Malformed)),
        ("v0.x.0", Err(Malformed)),
        ("v0.1.2.3", Err(Malformed)),
        ("v0..10", Err(Malformed)),
        ("v0.10.", Err(Malformed)),
        ("v.10.0", Err(Malformed)),
        ("v0.10.0-rc1", Err(Malformed)),
        ("v00.10.0", Err(Malformed)),
        ("v0.010.0", Err(Malformed)),
        ("v0.10.00", Err(Malformed)),
        ("v1.2.3.99999999999999999999", Err(Malformed)),
        ("v1.2.99999999999999999999", Err(Overflow)),
    ];
    for (tag, want) in rows {
        assert_eq!(&parse_release_tag(tag), want, "{tag}");
    }
    assert!(parse_release_tag("v0.2.9").unwrap() < parse_release_tag("v0.2.10").unwrap());
}

#[test]
fn canonical_version_pins_the_spelling_and_the_component_count() {
    assert_eq!(
        canonical_version("v0.2.0", &[0, 2, 0]),
        Some("0.2.0".to_string())
    );
    assert_eq!(canonical_version("v0.02.0", &[0, 2, 0]), None);
    assert_eq!(canonical_version("v0.2", &[0, 2]), None);
}
