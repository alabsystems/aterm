// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_missing_union_could_not_run_and_a_malformed_one_is_a_finding() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("libc-oracle")).expect("mkdir");
    let cell = &cells::CELLS[0];
    match union_names(dir.path(), cell) {
        Err(UnionStop::CouldNotRun(c)) => {
            assert!(c.what.contains(union::PATH), "{c}");
            assert!(c.fix.is_some_and(|f| f.contains("git checkout")));
        }
        other => panic!("a missing union.json must not run: {other:?}"),
    }
    std::fs::write(union::path(dir.path()), "{").expect("write");
    assert!(
        matches!(
            union_names(dir.path(), cell),
            Err(UnionStop::Finding(Finding::Input { .. }))
        ),
        "a malformed union.json is a finding"
    );
    std::fs::write(union::path(dir.path()), "{}").expect("write");
    assert!(
        matches!(
            union_names(dir.path(), cell),
            Err(UnionStop::Finding(Finding::Input { .. }))
        ),
        "a union.json without the cell's list is a finding"
    );
}
