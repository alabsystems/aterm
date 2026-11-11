// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::{FetchBatch, FetchRow};

fn row(source: &str, into: &str) -> FetchRow {
    FetchRow {
        source: source.into(),
        into: into.into(),
        expected_oid: "a".repeat(40),
    }
}

#[test]
fn object_imports_are_bounded_exact_unique_and_publish_no_refs() {
    let a = "a".repeat(40);
    let b = "b".repeat(40);
    let batch = FetchBatch::objects(vec![b.clone(), a.clone()]).unwrap();
    assert!(batch.rows().is_empty());
    assert_eq!(batch.object_ids(), &[a.clone(), b]);
    for invalid in [
        Vec::new(),
        vec!["0".repeat(40)],
        vec!["refs/heads/main".into()],
        vec![a.clone(), a.clone()],
        vec![a; super::FETCH_ROWS_MAX + 1],
    ] {
        assert!(FetchBatch::objects(invalid).is_err());
    }
}

#[test]
fn batch_names_identity_and_capacity_are_admitted_before_dispatch() {
    let valid = row("refs/heads/main", "refs/agh/main");
    assert!(FetchBatch::new(vec![valid.clone()]).is_ok());
    for invalid in [
        Vec::new(),
        vec![row("main", "refs/agh/main")],
        vec![row("refs/heads/*", "refs/agh/main")],
        vec![row("refs/heads/main", "refs/heads/main")],
        vec![valid.clone(), valid.clone()],
        vec![valid.clone(), row("refs/tags/v1", "refs/agh/main/child")],
        vec![row(&"b".repeat(40), "refs/agh/literal")],
        vec![valid.clone(); super::FETCH_ROWS_MAX + 1],
    ] {
        assert!(FetchBatch::new(invalid).is_err());
    }
    let mut invalid = valid;
    invalid.expected_oid = "0".repeat(40);
    assert!(FetchBatch::new(vec![invalid]).is_err());
    let oversized = row(
        &format!("refs/tags/{}", "v".repeat(super::FETCH_BYTES_MAX)),
        "refs/agh/tag",
    );
    assert!(
        FetchBatch::new(vec![oversized])
            .expect_err("byte bound")
            .why
            .contains("512 KiB")
    );
}
