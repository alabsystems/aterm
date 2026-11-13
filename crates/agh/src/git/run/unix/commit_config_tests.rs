// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;
use aterm_json::{Value, json};

fn verdict(input: &[u8]) -> Value {
    aterm_json::from_slice(&project(input)).unwrap()
}

#[test]
fn only_the_commit_projection_admits_paired_canonical_filter_metadata() {
    for remote in ["origin", "source.cache", "Origin"] {
        let values = format!(
            "core.bare\ntrue\0remote.{remote}.partialclonefilter\nblob:none\0remote.{remote}.promisor\ntrue\0"
        );
        assert_eq!(verdict(values.as_bytes()), json!({"Allowed":{"keys":3}}));
        let names =
            format!("core.bare\0remote.{remote}.partialclonefilter\0remote.{remote}.promisor\0");
        for read_only in [false, true] {
            assert_eq!(
                aterm_json::from_slice::<Value>(&project_config_names(names.as_bytes(), read_only))
                    .unwrap(),
                json!({"Refused":{"class":"Remote"}})
            );
        }
    }
}

#[test]
fn incomplete_duplicate_or_noncanonical_filter_metadata_is_refused() {
    for input in [
        "remote.origin.promisor\ntrue\0",
        "remote.origin.partialclonefilter\nblob:none\0",
        "remote.origin.promisor\nfalse\0remote.origin.partialclonefilter\nblob:none\0",
        "remote.origin.promisor\ntrue\0remote.origin.partialclonefilter\nblob:limit=1\0",
        "remote.origin.promisor\ntrue\0remote.origin.partialclonefilter\ntree:0\0",
        "remote.origin.promisor\ntrue\0remote.other.partialclonefilter\nblob:none\0",
        "remote.origin.promisor\ntrue\0remote.origin.promisor\ntrue\0remote.origin.partialclonefilter\nblob:none\0",
        "remote.origin.promisor\ntrue\0remote.origin.partialclonefilter\nblob:none\0remote.origin.partialclonefilter\nblob:none\0",
        "remote.origin.promisor\0remote.origin.partialclonefilter\nblob:none\0",
        "remote.https://github.com/x.promisor\ntrue\0remote.https://github.com/x.partialclonefilter\nblob:none\0",
        "remote.origin.promisor\ntrue\nprivate-value\0remote.origin.partialclonefilter\nblob:none\0",
    ] {
        assert_eq!(
            verdict(input.as_bytes()),
            json!({"Refused":{"class":"Remote"}}),
            "{input:?}"
        );
    }
}

#[test]
fn filter_metadata_does_not_admit_any_other_guarded_configuration() {
    let prefix = "remote.origin.promisor\ntrue\0remote.origin.partialclonefilter\nblob:none\0";
    for (key, class) in [
        ("remote.origin.uploadpack", "Remote"),
        ("remote.origin.proxy", "Remote"),
        ("core.hookspath", "Core"),
        ("core.alternaterefscommand", "Core"),
        ("http.sslverify", "Http"),
        ("url.https://github.com/.insteadof", "Url"),
        ("include.path", "Include"),
        ("extensions.partialclone", "Extensions"),
        ("protocol.file.allow", "Protocol"),
    ] {
        let input = format!("{prefix}{key}\nprivate-value\0");
        assert_eq!(
            verdict(input.as_bytes()),
            json!({"Refused":{"class":class}})
        );
        assert!(
            !String::from_utf8(project(input.as_bytes()))
                .unwrap()
                .contains("private-value")
        );
    }
}

#[test]
fn truncated_or_oversized_value_inventory_never_becomes_admission() {
    assert_eq!(
        verdict(b"core.bare\ntrue"),
        json!({"Refused":{"class":"Unknown"}})
    );
    assert_eq!(
        verdict(b"core.bare\ntrue\0\0"),
        json!({"Refused":{"class":"Unknown"}})
    );
    let input = vec![b'x'; SCREEN_MAX + 1];
    assert_eq!(verdict(&input), json!({"TooLong":{"bytes":input.len()}}));
}
